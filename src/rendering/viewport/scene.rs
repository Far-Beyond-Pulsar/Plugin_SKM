//! Pure viewport geometry: turns a skeleton pose into the meshes, lines and
//! markers the renderer draws, and builds the orientation gizmo.
//!
//! Nothing here touches GPUI or wgpu, so it is unit-testable and can be
//! rendered headlessly.

use std::collections::{HashMap, HashSet};

use crate::core::{bone_world_transform, evaluate_world_transforms, AnimationClip, Skeleton, Vec3};
use crate::editor::panel::ViewOptions;
use crate::style::{mix, with_alpha, Palette};

use super::camera::{OrbitCamera, ViewAxis};
use super::types::*;

/// Fraction of a bone's length used as the octahedron's "neck" ring distance
/// from the head joint, and as its radius.
const BONE_RING_DIST_FRAC: f32 = 0.12;
const BONE_RING_RADIUS_FRAC: f32 = 0.125;
/// Radius of the sphere capping each bone's tip, as a fraction of its length.
const BONE_TIP_SPHERE_FRAC: f32 = 0.075;
const SPHERE_RINGS: usize = 8;
const SPHERE_SEGMENTS: usize = 14;
/// Onion skin: poses shown either side of the playhead.
const ONION_STEPS: i32 = 2;

/// Recorded tracer positions per bone id.
pub type TracerTrails = HashMap<String, Vec<Vec3>>;
/// Tracer color per bone id.
pub type TracerColors = HashMap<String, [f32; 4]>;

pub struct SceneInput<'a> {
    pub skeleton: &'a Skeleton,
    pub clip: &'a AnimationClip,
    pub time: f32,
    /// Play range in seconds, used to bound the motion path and onion skin.
    pub range: (f32, f32),
    pub selected: Option<&'a str>,
    pub hovered: Option<&'a str>,
    pub view: ViewOptions,
    pub palette: &'a Palette,
    /// Recorded tracer trails and their colors, when tracers are on.
    pub tracers: Option<(&'a TracerTrails, &'a TracerColors)>,
}

#[derive(Default)]
pub struct SceneGeometry {
    pub mesh: Vec<MeshVertex>,
    pub ghost: Vec<MeshVertex>,
    pub ribbons: Vec<RibbonSegment>,
    pub joints: Vec<JointInstance>,
    /// World position of every bone's joint, for picking and labels.
    pub joint_positions: Vec<(String, Vec3)>,
    pub center: Vec3,
    pub radius: f32,
    /// Lowest joint height, for fading the contact shadow as the rig lifts.
    pub min_y: f32,
}

impl SceneGeometry {
    /// Contact shadow parameters for the ground shader: `x`, `z` center,
    /// `y` radius, `w` strength.
    pub fn shadow(&self, palette: &Palette) -> [f32; 4] {
        let radius = (self.radius * 1.1).max(0.45);
        let lift = (self.min_y.max(0.0) / (radius * 1.2)).min(4.0);
        let base = if palette.dark { 0.55 } else { 0.28 };
        [self.center.x, self.center.z, radius, base * (-lift).exp()]
    }
}

fn octahedron_frame(head: Vec3, tail: Vec3) -> Option<(Vec3, Vec3, Vec3, f32)> {
    let diff = tail.sub(head);
    let length = diff.length();
    if length < 1e-5 {
        return None;
    }
    let axis = diff.scale(1.0 / length);
    let world_up = Vec3::new(0.0, 1.0, 0.0);
    let reference = if axis.dot(world_up).abs() > 0.99 {
        Vec3::new(1.0, 0.0, 0.0)
    } else {
        world_up
    };
    let right = axis.cross(reference).normalize();
    let up = right.cross(axis).normalize();
    Some((axis, right, up, length))
}

/// Append the triangles of a classic octahedral bone spanning `head` (the
/// parent joint) to `tail`, capped with a sphere at the tail. When `edges` is
/// given, the shape's outline is added as ribbon segments.
fn push_bone(
    mesh: &mut Vec<MeshVertex>,
    edges: Option<(&mut Vec<RibbonSegment>, f32, [f32; 4])>,
    head: Vec3,
    tail: Vec3,
    color: [f32; 4],
    glow: f32,
) {
    let Some((axis, right, up, length)) = octahedron_frame(head, tail) else {
        return;
    };

    let ring_center = head.add(axis.scale(length * BONE_RING_DIST_FRAC));
    let radius = length * BONE_RING_RADIUS_FRAC;
    let ring = [
        ring_center.add(right.scale(radius)),
        ring_center.add(up.scale(radius)),
        ring_center.sub(right.scale(radius)),
        ring_center.sub(up.scale(radius)),
    ];

    for i in 0..4 {
        let (b, c) = (ring[i], ring[(i + 1) % 4]);
        for (apex, v0, v1) in [(head, b, c), (tail, c, b)] {
            let normal = v0.sub(apex).cross(v1.sub(apex)).normalize().to_array();
            for p in [apex, v0, v1] {
                mesh.push(MeshVertex {
                    pos: p.to_array(),
                    normal,
                    color,
                    glow,
                });
            }
        }
    }
    push_sphere(mesh, tail, length * BONE_TIP_SPHERE_FRAC, color, glow);

    if let Some((out, width, edge_color)) = edges {
        for i in 0..4 {
            let (b, c) = (ring[i], ring[(i + 1) % 4]);
            for (a, z) in [(head, b), (tail, b), (b, c)] {
                out.push(
                    RibbonSegment::solid(a.to_array(), z.to_array(), width, edge_color).with_bias(0.0008),
                );
            }
        }
    }
}

/// Append a UV sphere of `radius` centered at `center`.
fn push_sphere(mesh: &mut Vec<MeshVertex>, center: Vec3, radius: f32, color: [f32; 4], glow: f32) {
    use std::f32::consts::PI;
    let point = |lat: f32, lon: f32| Vec3::new(lat.cos() * lon.cos(), lat.sin(), lat.cos() * lon.sin());

    for i in 0..SPHERE_RINGS {
        let lat0 = PI * (i as f32 / SPHERE_RINGS as f32 - 0.5);
        let lat1 = PI * ((i + 1) as f32 / SPHERE_RINGS as f32 - 0.5);
        for j in 0..SPHERE_SEGMENTS {
            let lon0 = 2.0 * PI * (j as f32 / SPHERE_SEGMENTS as f32);
            let lon1 = 2.0 * PI * ((j + 1) as f32 / SPHERE_SEGMENTS as f32);
            let (p00, p01) = (point(lat0, lon0), point(lat0, lon1));
            let (p10, p11) = (point(lat1, lon0), point(lat1, lon1));
            for tri in [[p00, p10, p11], [p00, p11, p01]] {
                for p in tri {
                    mesh.push(MeshVertex {
                        pos: center.add(p.scale(radius)).to_array(),
                        normal: p.to_array(),
                        color,
                        glow,
                    });
                }
            }
        }
    }
}

/// Build all viewport geometry for the current pose.
pub fn build_scene(input: &SceneInput) -> SceneGeometry {
    let p = input.palette;
    let skeleton = input.skeleton;
    let mut geo = SceneGeometry::default();

    let world = evaluate_world_transforms(skeleton, input.clip, input.time);
    let position = |id: &str| world.get(id).map(|m| m.transform_point(Vec3::ZERO));

    // Bones on the path from the selection up to the root get a faint tint.
    let mut chain: HashSet<&str> = HashSet::new();
    if let Some(selected) = input.selected {
        let mut current = skeleton.bone(selected).and_then(|b| b.parent.as_deref());
        while let Some(id) = current {
            if !chain.insert(id) {
                break;
            }
            current = skeleton.bone(id).and_then(|b| b.parent.as_deref());
        }
    }
    let animated: HashSet<&str> = input.clip.tracks.iter().map(|t| t.bone_id.as_str()).collect();

    let neutral = mix(p.text, p.info, 0.10);
    let bone_base = if p.dark {
        mix(neutral, p.background, 0.22)
    } else {
        mix(neutral, p.background, 0.55)
    };
    let idle = mix(bone_base, p.background, 0.26);
    let animated_tint = mix(bone_base, p.info, 0.22);
    let outline = with_alpha(
        if p.dark {
            mix(p.background, [0.0, 0.0, 0.0, 1.0], 0.55)
        } else {
            mix(p.text, p.background, 0.35)
        },
        if p.dark { 0.7 } else { 0.5 },
    );
    let selected_edge = with_alpha(mix(p.warning, [1.0, 1.0, 1.0, 1.0], 0.5), 1.0);
    let hover_edge = with_alpha(mix(p.info, [1.0, 1.0, 1.0, 1.0], 0.4), 1.0);

    let tracer_colors = input.tracers.map(|(_, colors)| colors);

    for bone in &skeleton.bones {
        let Some(pos) = position(&bone.id) else { continue };
        geo.joint_positions.push((bone.id.clone(), pos));
        let is_selected = input.selected == Some(bone.id.as_str());
        let is_hovered = input.hovered == Some(bone.id.as_str());

        let mut color = if animated.contains(bone.id.as_str()) {
            animated_tint
        } else {
            idle
        };
        if chain.contains(bone.id.as_str()) {
            color = mix(color, p.warning, 0.28);
        }
        if let Some(colors) = tracer_colors {
            if let Some(c) = colors.get(&bone.id) {
                color = *c;
            }
        }
        let (glow, edge) = if is_selected {
            color = p.warning;
            (0.30, Some((1.9, selected_edge)))
        } else if is_hovered {
            color = mix(color, p.info, 0.55);
            (0.16, Some((1.6, hover_edge)))
        } else {
            (0.0, Some((1.0, outline)))
        };

        if !input.view.bones {
            continue;
        }
        match bone.parent.as_deref().and_then(position) {
            Some(parent_pos) => {
                let edges = edge.map(|(w, c)| (&mut geo.ribbons, w, c));
                push_bone(&mut geo.mesh, edges, parent_pos, pos, color, glow);
            }
            None => {
                // A root has no parent to span from; mark it with a sphere
                // sized to its children.
                let child_len: Vec<f32> = skeleton
                    .children_of(&bone.id)
                    .iter()
                    .filter_map(|c| position(&c.id).map(|cp| cp.sub(pos).length()))
                    .collect();
                let avg = if child_len.is_empty() {
                    0.3
                } else {
                    child_len.iter().sum::<f32>() / child_len.len() as f32
                };
                push_sphere(&mut geo.mesh, pos, (avg * 0.16).clamp(0.02, 0.12), color, glow);
            }
        }
    }

    // Bounds for framing and the shadow.
    let points: Vec<Vec3> = geo.joint_positions.iter().map(|(_, p)| *p).collect();
    if let Some((center, radius)) = super::camera::bounding_sphere(&points) {
        geo.center = center;
        geo.radius = radius;
        geo.min_y = points.iter().map(|p| p.y).fold(f32::MAX, f32::min);
    }

    // Joint markers.
    if input.view.joints {
        for (id, pos) in &geo.joint_positions {
            if input.selected == Some(id.as_str()) || input.hovered == Some(id.as_str()) {
                continue;
            }
            geo.joints.push(JointInstance {
                center: pos.to_array(),
                size: 6.5,
                color: with_alpha(mix(p.text, p.background, 0.15), 0.92),
                style: JOINT_PIVOT,
            });
        }
    }
    for (id, pos) in &geo.joint_positions {
        if input.selected == Some(id.as_str()) {
            geo.joints.push(JointInstance {
                center: pos.to_array(),
                size: 24.0,
                color: p.warning,
                style: JOINT_SELECTED,
            });
        } else if input.hovered == Some(id.as_str()) {
            geo.joints.push(JointInstance {
                center: pos.to_array(),
                size: 20.0,
                color: mix(p.info, [1.0, 1.0, 1.0, 1.0], 0.3),
                style: JOINT_HOVER,
            });
        }
    }

    // Onion skin: faint poses before (warm) and after (cool) the playhead.
    if input.view.onion_skin {
        let fps = input.clip.fps_clamped();
        let step = ((fps / 12.0).round().max(1.0)) / fps;
        let (start, end) = input.range;
        for dir in [-1.0_f32, 1.0] {
            for k in 1..=ONION_STEPS {
                let t = input.time + dir * k as f32 * step;
                if t < start - 1e-4 || t > end + 1e-4 {
                    continue;
                }
                let ghost_world = evaluate_world_transforms(skeleton, input.clip, t);
                let tint = if dir < 0.0 { p.danger } else { p.success };
                let alpha = 0.32 * (1.0 - (k - 1) as f32 * 0.35);
                let color = with_alpha(mix(tint, [1.0, 1.0, 1.0, 1.0], 0.12), alpha);
                for bone in &skeleton.bones {
                    let (Some(pm), Some(m)) = (
                        bone.parent.as_deref().and_then(|id| ghost_world.get(id)),
                        ghost_world.get(&bone.id),
                    ) else {
                        continue;
                    };
                    push_bone(
                        &mut geo.ghost,
                        None,
                        pm.transform_point(Vec3::ZERO),
                        m.transform_point(Vec3::ZERO),
                        color,
                        0.0,
                    );
                }
            }
        }
    }

    // Motion path of the selected bone through the play range.
    if input.view.motion_path {
        if let Some(selected) = input.selected {
            push_motion_path(&mut geo, input, selected);
        }
    }

    // Recorded tracer trails, fading with age.
    if let Some((trails, colors)) = input.tracers {
        for (id, trail) in trails {
            let base = colors.get(id).copied().unwrap_or([0.3, 1.0, 0.4, 0.85]);
            let n = trail.len().max(2) as f32 - 1.0;
            for (i, pair) in trail.windows(2).enumerate() {
                let (t0, t1) = (i as f32 / n, (i + 1) as f32 / n);
                geo.ribbons.push(RibbonSegment {
                    a: pair[0].to_array(),
                    width: 1.8,
                    b: pair[1].to_array(),
                    bias: 0.0,
                    color_a: with_alpha(base, 0.08 + 0.80 * t0 * t0),
                    color_b: with_alpha(base, 0.08 + 0.80 * t1 * t1),
                });
            }
        }
    }

    geo
}

fn push_motion_path(geo: &mut SceneGeometry, input: &SceneInput, bone_id: &str) {
    let p = input.palette;
    let (start, end) = input.range;
    let fps = input.clip.fps_clamped();
    if end - start < 1e-4 {
        return;
    }
    // At most ~1 sample per frame, capped so a huge range stays cheap.
    let frames = (((end - start) * fps).ceil() as usize).clamp(2, 600);
    let sample = |t: f32| {
        bone_world_transform(input.skeleton, input.clip, bone_id, t).map(|m| m.transform_point(Vec3::ZERO))
    };

    let early = with_alpha(mix(p.info, [1.0, 1.0, 1.0, 1.0], 0.2), 0.85);
    let late = with_alpha(p.warning, 0.95);
    let mut prev = sample(start);
    for i in 1..=frames {
        let f = i as f32 / frames as f32;
        let cur = sample(start + (end - start) * f);
        if let (Some(a), Some(b)) = (prev, cur) {
            let f0 = (i - 1) as f32 / frames as f32;
            geo.ribbons.push(RibbonSegment {
                a: a.to_array(),
                width: 2.2,
                b: b.to_array(),
                bias: 0.0004,
                color_a: mix(early, late, f0),
                color_b: mix(early, late, f),
            });
        }
        prev = cur;
    }

    if let Some(track) = input.clip.track(bone_id) {
        for kf in &track.keyframes {
            if kf.time < start - 1e-4 || kf.time > end + 1e-4 {
                continue;
            }
            if let Some(pos) = sample(kf.time) {
                geo.joints.push(JointInstance {
                    center: pos.to_array(),
                    size: 10.0,
                    color: [1.0, 0.95, 0.85, 1.0],
                    style: JOINT_KEY,
                });
            }
        }
    }
}

// ── Orientation gizmo ──────────────────────────────────────────────────────

pub const GIZMO_SIZE_PX: f32 = 96.0;
pub const GIZMO_MARGIN_PX: f32 = 14.0;
const GIZMO_AXIS_LEN: f32 = 0.78;
const GIZMO_BUBBLE_PX: f32 = 18.0;
const GIZMO_NEG_BUBBLE_PX: f32 = 13.0;
const GIZMO_BG_PX: f32 = GIZMO_SIZE_PX + GIZMO_BUBBLE_PX + 8.0;

/// Where the gizmo's clickable bubbles are, in surface pixels.
#[derive(Default, Clone)]
pub struct GizmoLayout {
    pub bubbles: Vec<(ViewAxis, (f32, f32), f32)>,
}

impl GizmoLayout {
    /// The axis bubble under surface pixel `pos`, nearest first.
    pub fn hit(&self, pos: (f32, f32)) -> Option<ViewAxis> {
        self.bubbles
            .iter()
            .filter_map(|(axis, c, r)| {
                let d = ((c.0 - pos.0).powi(2) + (c.1 - pos.1).powi(2)).sqrt();
                (d <= r + 3.0).then_some((*axis, d))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(axis, _)| axis)
    }
}

pub struct Gizmo {
    pub ribbons: Vec<RibbonSegment>,
    /// Backdrop disc first, then bubbles back to front.
    pub bubbles: Vec<GizmoBubbleInstance>,
    pub layout: GizmoLayout,
}

/// Build the orientation gizmo for a `w`×`h` surface. Positions are
/// pre-projected to NDC (the gizmo pass uses an identity view-projection).
pub fn build_gizmo(camera: &OrbitCamera, w: f32, h: f32, palette: &Palette) -> Gizmo {
    let view_dir = camera.eye_dir();
    let (right, up) = camera.right_up();

    let half_px = GIZMO_SIZE_PX * 0.5;
    let bg_radius = GIZMO_BG_PX * 0.5;
    let cx_px = w - GIZMO_MARGIN_PX - bg_radius;
    let cy_px = GIZMO_MARGIN_PX + bg_radius;
    let to_ndc = |x: f32, y: f32| [(x / w) * 2.0 - 1.0, 1.0 - (y / h) * 2.0, 0.5];
    let center_ndc = to_ndc(cx_px, cy_px);

    let colors = |axis: ViewAxis| match axis {
        ViewAxis::PosX | ViewAxis::NegX => palette.axis_x,
        ViewAxis::PosY | ViewAxis::NegY => palette.axis_y,
        ViewAxis::PosZ | ViewAxis::NegZ => palette.axis_z,
    };

    // Screen position (px) of each axis tip.
    let tip_px = |axis: ViewAxis| {
        let d = axis.direction().scale(GIZMO_AXIS_LEN);
        (
            cx_px + d.dot(right) * (half_px / GIZMO_AXIS_LEN),
            cy_px - d.dot(up) * (half_px / GIZMO_AXIS_LEN),
        )
    };

    // Far axes first so nearer bubbles land on top.
    let mut order: Vec<ViewAxis> = ViewAxis::ALL.to_vec();
    order.sort_by(|a, b| {
        a.direction()
            .dot(view_dir)
            .total_cmp(&b.direction().dot(view_dir))
    });

    let mut ribbons = Vec::new();
    let mut bubbles = vec![GizmoBubbleInstance {
        center: center_ndc,
        size: GIZMO_BG_PX,
        color: with_alpha(
            mix(palette.panel, palette.background, 0.3),
            if palette.dark { 0.66 } else { 0.78 },
        ),
        letter: -1.0,
        kind: BUBBLE_BACKDROP,
    }];
    let mut layout = GizmoLayout::default();

    for axis in order {
        let (tx, ty) = tip_px(axis);
        let positive = matches!(axis, ViewAxis::PosX | ViewAxis::PosY | ViewAxis::PosZ);
        let size = if positive {
            GIZMO_BUBBLE_PX
        } else {
            GIZMO_NEG_BUBBLE_PX
        };
        let color = colors(axis);
        if positive {
            ribbons.push(RibbonSegment::solid(
                center_ndc,
                to_ndc(tx, ty),
                2.4,
                with_alpha(color, 0.95),
            ));
        }
        bubbles.push(GizmoBubbleInstance {
            center: to_ndc(tx, ty),
            size,
            color: with_alpha(color, if positive { 1.0 } else { 0.85 }),
            letter: if positive {
                match axis {
                    ViewAxis::PosX => 0.0,
                    ViewAxis::PosY => 1.0,
                    _ => 2.0,
                }
            } else {
                -1.0
            },
            kind: if positive { BUBBLE_FILLED } else { BUBBLE_HOLLOW },
        });
        layout.bubbles.push((axis, (tx, ty), size * 0.5));
    }

    Gizmo {
        ribbons,
        bubbles,
        layout,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::sample;

    fn input<'a>(
        skeleton: &'a Skeleton,
        clip: &'a AnimationClip,
        palette: &'a Palette,
        selected: Option<&'a str>,
    ) -> SceneInput<'a> {
        SceneInput {
            skeleton,
            clip,
            time: 0.2,
            range: (0.0, clip.duration),
            selected,
            hovered: None,
            view: ViewOptions {
                onion_skin: true,
                ..ViewOptions::default()
            },
            palette,
            tracers: None,
        }
    }

    #[test]
    fn scene_has_geometry_for_every_bone_and_stays_finite() {
        let skeleton = sample::humanoid_skeleton();
        let clip = sample::run_animation(&skeleton);
        let palette = Palette::test_dark();
        let geo = build_scene(&input(&skeleton, &clip, &palette, Some("thigh_r")));

        assert_eq!(geo.joint_positions.len(), skeleton.bones.len());
        assert!(geo.mesh.len() > skeleton.bones.len() * 24);
        assert!(!geo.ghost.is_empty(), "onion skin should add ghost geometry");
        assert!(!geo.ribbons.is_empty());
        assert!(geo.radius > 0.2);
        let finite = |v: &[f32]| v.iter().all(|x| x.is_finite());
        assert!(geo.mesh.iter().all(|v| finite(&v.pos) && finite(&v.normal)));
        assert!(geo.ribbons.iter().all(|r| finite(&r.a) && finite(&r.b)));
    }

    #[test]
    fn selecting_a_bone_adds_a_marker_and_motion_path() {
        let skeleton = sample::humanoid_skeleton();
        let clip = sample::run_animation(&skeleton);
        let palette = Palette::test_dark();
        let none = build_scene(&input(&skeleton, &clip, &palette, None));
        let some = build_scene(&input(&skeleton, &clip, &palette, Some("hand_l")));
        assert!(some.joints.iter().any(|j| j.style == JOINT_SELECTED));
        assert!(!none.joints.iter().any(|j| j.style == JOINT_SELECTED));
        assert!(
            some.joints.iter().any(|j| j.style == JOINT_KEY),
            "keyframes mark the path"
        );
        assert!(some.ribbons.len() > none.ribbons.len());
    }

    #[test]
    fn gizmo_bubbles_are_hit_testable() {
        let cam = OrbitCamera::default();
        let gizmo = build_gizmo(&cam, 800.0, 600.0, &Palette::test_dark());
        assert_eq!(gizmo.bubbles.len(), 7, "backdrop + six axes");
        for (axis, center, _) in gizmo.layout.bubbles.clone() {
            assert_eq!(gizmo.layout.hit(center), Some(axis));
        }
        assert_eq!(gizmo.layout.hit((5.0, 590.0)), None);
    }
}
