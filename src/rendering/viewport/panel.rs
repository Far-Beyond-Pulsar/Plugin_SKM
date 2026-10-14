//! `ViewportPanel` — dock panel hosting the 3D bone viewport.
//!
//! The scene is drawn by [`ViewportRenderer`] into a WGPU surface; the
//! controls floating over it (view toggles, camera buttons, bone labels, HUD)
//! are GPUI components from the shared UI crate.
//!
//! | Input                | Action                                   |
//! |----------------------|------------------------------------------|
//! | RMB + drag           | Look (FPS-style, eye stays fixed)        |
//! | RMB held + WASD/Space/Shift | Fly                               |
//! | MMB + drag           | Pan                                      |
//! | Alt + LMB + drag     | Orbit around the target                  |
//! | Alt + RMB + drag     | Dolly                                    |
//! | Scroll wheel         | Zoom                                     |
//! | LMB click            | Select the bone under the cursor         |
//! | Double-click a bone  | Frame it                                 |
//! | Click a gizmo axis   | Snap the view to that axis               |
//! | F / Home             | Frame selection / frame everything       |
//! | Space                | Play / pause                             |

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder;
use gpui::*;
use pulsar_physics::SkeletalPhysicsAsset;
use ui::button::{Button, ButtonVariants};
use ui::tag::Tag;
use ui::{
    dock::PanelEvent, h_flex, ActiveTheme, Icon, IconName, PixelsExt, Selectable, Sizable,
    StyledExt,
};

use crate::core::{Mat4, Vec3};
use crate::editor::panel::{SkeletalAnimEditorPanel, ViewOptions};
use crate::style::Palette;
use crate::ui_components::chrome::{toolbar_divider, MONO};

use super::camera::{FlyInput, OrbitCamera, MAX_DISTANCE, MIN_DISTANCE};
use super::renderer::{SceneFrame, ViewportRenderer};
use super::scene::{build_gizmo, build_scene, GizmoLayout, SceneInput, SkinInput};
use super::types::ViewportUniforms;

/// Hit-test radius, in pixels, used to pick the nearest joint.
const PICK_RADIUS_PX: f32 = 14.0;

/// Which drag gesture is currently active.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum DragMode {
    #[default]
    None,
    /// RMB drag — FPS-style look: eye stays fixed, target follows the view direction.
    Look,
    /// MMB drag — pan: translate both eye and target in screen space.
    Pan,
    /// Alt + LMB drag — orbit: eye orbits around fixed target.
    Orbit,
    /// Alt + RMB drag — zoom/dolly: adjust distance to target.
    Zoom,
}

pub struct ViewportPanel {
    editor: WeakEntity<SkeletalAnimEditorPanel>,
    focus_handle: FocusHandle,
    renderer: ViewportRenderer,
    surface: Option<WgpuSurfaceHandle>,
    camera: OrbitCamera,
    /// Camera the view is easing toward (framing / axis snaps).
    camera_goal: Option<OrbitCamera>,
    drag_last: Option<Point<f32>>,
    drag_mode: DragMode,
    keys: FlyInput,
    /// True until the first render where the skeleton's bounds are known.
    needs_fit: bool,
    /// View-projection and panel bounds from the most recent paint, used to
    /// project joints for picking and labels.
    last_view_proj: Mat4,
    last_origin: Point<f32>,
    last_size: Size<f32>,
    /// Joint positions and overall bounds from the last built scene.
    joints: Vec<(String, Vec3)>,
    bounds: (Vec3, f32),
    gizmo_layout: GizmoLayout,
    palette: Palette,
    physics_cache_key: Option<(PathBuf, String)>,
    physics_asset: Option<SkeletalPhysicsAsset>,
    physics_cache_checked: Instant,
}

impl ViewportPanel {
    pub fn new(editor: WeakEntity<SkeletalAnimEditorPanel>, cx: &mut Context<Self>) -> Self {
        let panel = Self {
            editor,
            focus_handle: cx.focus_handle(),
            renderer: ViewportRenderer::new(),
            surface: None,
            camera: OrbitCamera::default(),
            camera_goal: None,
            drag_last: None,
            drag_mode: DragMode::None,
            keys: FlyInput::default(),
            needs_fit: true,
            last_view_proj: Mat4::IDENTITY,
            last_origin: Point::new(0.0, 0.0),
            last_size: Size::new(1.0, 1.0),
            joints: Vec::new(),
            bounds: (Vec3::ZERO, 1.0),
            gizmo_layout: GizmoLayout::default(),
            palette: Palette::from_theme(cx.theme()),
            physics_cache_key: None,
            physics_asset: None,
            physics_cache_checked: Instant::now() - Duration::from_secs(2),
        };

        let weak = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            const FRAME: std::time::Duration = std::time::Duration::from_millis(16);
            loop {
                smol::Timer::after(FRAME).await;
                let alive = weak.update(cx, |panel, cx| {
                    panel.tick(cx);
                    true
                });
                if !matches!(alive, Ok(true)) {
                    break;
                }
            }
        })
        .detach();

        panel
    }

    // ── Input ──────────────────────────────────────────────────────────────

    fn local(&self, position: Point<Pixels>) -> (f32, f32) {
        (
            position.x.as_f32() - self.last_origin.x,
            position.y.as_f32() - self.last_origin.y,
        )
    }

    fn handle_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window, cx);
        self.camera_goal = None;
        let pos = Point::new(event.position.x.as_f32(), event.position.y.as_f32());

        match event.button {
            MouseButton::Right => {
                self.drag_mode = if event.modifiers.alt {
                    DragMode::Zoom
                } else {
                    DragMode::Look
                };
                self.drag_last = Some(pos);
            }
            MouseButton::Middle => {
                self.drag_mode = DragMode::Pan;
                self.drag_last = Some(pos);
            }
            MouseButton::Left if event.modifiers.alt => {
                self.drag_mode = DragMode::Orbit;
                self.drag_last = Some(pos);
            }
            MouseButton::Left => {
                let local = self.local(event.position);
                if let Some(axis) = self.gizmo_layout.hit(local) {
                    let mut goal = self.camera;
                    goal.snap_to(axis);
                    self.camera_goal = Some(goal);
                    cx.notify();
                    return;
                }
                self.pick_bone(local, event.click_count, window, cx);
            }
            _ => {}
        }
    }

    fn handle_mouse_up(&mut self) {
        self.drag_last = None;
        if self.drag_mode == DragMode::Look {
            self.keys = FlyInput::default();
        }
        self.drag_mode = DragMode::None;
    }

    fn set_key(&mut self, key: &str, down: bool) -> bool {
        match key {
            "w" | "W" => self.keys.forward = down,
            "s" | "S" => self.keys.backward = down,
            "a" | "A" => self.keys.left = down,
            "d" | "D" => self.keys.right = down,
            "space" | " " => self.keys.up = down,
            _ => return false,
        }
        true
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        // While flying, WASD/Space move the camera.
        if self.drag_mode == DragMode::Look {
            if self.set_key(key, true) {
                cx.notify();
            }
            return;
        }
        match key {
            "f" => self.frame_selected(cx),
            "home" => self.frame_all(),
            "space" | " " => {
                if let Some(editor) = self.editor.upgrade() {
                    editor.update(cx, |e, cx| e.toggle_play(cx));
                }
            }
            "left" | "right" => {
                let delta = if key == "left" { -1 } else { 1 };
                if let Some(editor) = self.editor.upgrade() {
                    editor.update(cx, |e, cx| e.step_frame(delta, window, cx));
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn handle_key_up(&mut self, event: &KeyUpEvent, cx: &mut Context<Self>) {
        if self.set_key(event.keystroke.key.as_str(), false) {
            cx.notify();
        }
    }

    /// Shift is a modifier rather than a key event, so track it separately to
    /// drive "fly down" while in Look mode.
    fn handle_modifiers_changed(&mut self, event: &ModifiersChangedEvent, cx: &mut Context<Self>) {
        if self.drag_mode != DragMode::Look {
            return;
        }
        if event.modifiers.shift != self.keys.down {
            self.keys.down = event.modifiers.shift;
            cx.notify();
        }
    }

    /// Per-frame update: fly movement and camera easing.
    fn tick(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;

        if self.drag_mode == DragMode::Look {
            let keys = self.keys;
            changed |= self.camera.fly(&keys);
        }

        if let Some(goal) = self.camera_goal {
            const EASE: f32 = 0.22;
            let delta_yaw =
                ((goal.yaw_deg - self.camera.yaw_deg + 540.0).rem_euclid(360.0)) - 180.0;
            self.camera.yaw_deg += delta_yaw * EASE;
            self.camera.pitch_deg += (goal.pitch_deg - self.camera.pitch_deg) * EASE;
            self.camera.distance += (goal.distance - self.camera.distance) * EASE;
            self.camera.target = self.camera.target.lerp(goal.target, EASE);
            let settled = delta_yaw.abs() < 0.05
                && (goal.pitch_deg - self.camera.pitch_deg).abs() < 0.05
                && (goal.distance - self.camera.distance).abs() < 0.002
                && goal.target.sub(self.camera.target).length() < 0.0005;
            if settled {
                self.camera = goal;
                self.camera_goal = None;
            }
            changed = true;
        }

        if changed {
            cx.notify();
        }
    }

    fn handle_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let local = self.local(event.position);

        let Some(last) = self.drag_last else {
            self.update_hover(local, cx);
            return;
        };
        if self.drag_mode == DragMode::None {
            return;
        }
        // Button released outside the panel: end the gesture.
        if event.pressed_button.is_none() {
            self.handle_mouse_up();
            return;
        }

        let pos = Point::new(event.position.x.as_f32(), event.position.y.as_f32());
        let delta = Point::new(pos.x - last.x, pos.y - last.y);
        self.drag_last = Some(pos);

        match self.drag_mode {
            DragMode::Look => self.camera.look(delta.x, delta.y),
            DragMode::Pan => self.camera.pan(delta.x, delta.y),
            DragMode::Orbit => self.camera.orbit(delta.x, delta.y),
            DragMode::Zoom => self.camera.dolly(delta.y),
            DragMode::None => {}
        }
        cx.notify();
    }

    fn handle_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let delta = match event.delta {
            ScrollDelta::Lines(p) => p.y,
            ScrollDelta::Pixels(p) => p.y.as_f32() / 40.0,
        };
        self.camera_goal = None;
        self.camera.distance =
            (self.camera.distance * (1.0 - delta * 0.1)).clamp(MIN_DISTANCE, MAX_DISTANCE);
        cx.notify();
    }

    // ── Picking and projection ─────────────────────────────────────────────

    /// Panel-local pixel position of a world point, if it is in front of the
    /// camera.
    fn project(&self, p: Vec3) -> Option<(f32, f32)> {
        let (x, y, _, w) = self.last_view_proj.transform_clip(p);
        if w <= 0.0 {
            return None;
        }
        Some((
            (x / w * 0.5 + 0.5) * self.last_size.width.max(1.0),
            (1.0 - (y / w * 0.5 + 0.5)) * self.last_size.height.max(1.0),
        ))
    }

    /// The joint nearest to `pos` within the pick radius.
    fn nearest_joint(&self, pos: (f32, f32)) -> Option<&str> {
        self.joints
            .iter()
            .filter_map(|(id, p)| {
                let (sx, sy) = self.project(*p)?;
                let d = ((sx - pos.0).powi(2) + (sy - pos.1).powi(2)).sqrt();
                (d <= PICK_RADIUS_PX).then_some((id.as_str(), d))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id)
    }

    fn pick_bone(
        &mut self,
        pos: (f32, f32),
        clicks: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.editor.upgrade() else {
            return;
        };
        let hit = self.nearest_joint(pos).map(str::to_string);
        let double = clicks >= 2 && hit.is_some();
        editor.update(cx, |e, cx| e.select_bone(hit, window, cx));
        if double {
            self.frame_selected(cx);
        }
    }

    fn update_hover(&mut self, pos: (f32, f32), cx: &mut Context<Self>) {
        let Some(editor) = self.editor.upgrade() else {
            return;
        };
        let hover = if self.gizmo_layout.hit(pos).is_some() {
            None
        } else {
            self.nearest_joint(pos).map(str::to_string)
        };
        if editor.read(cx).hovered_bone != hover {
            editor.update(cx, |e, cx| {
                e.hovered_bone = hover;
                cx.notify();
            });
        }
    }

    // ── Camera framing ─────────────────────────────────────────────────────

    fn frame_all(&mut self) {
        let mut goal = self.camera;
        goal.frame_sphere(self.bounds.0, self.bounds.1);
        self.camera_goal = Some(goal);
    }

    fn frame_selected(&mut self, cx: &mut Context<Self>) {
        let selected = self
            .editor
            .upgrade()
            .and_then(|e| e.read(cx).selected_bone.clone());
        let Some(id) = selected else {
            self.frame_all();
            return;
        };
        let Some((_, pos)) = self.joints.iter().find(|(b, _)| *b == id) else {
            self.frame_all();
            return;
        };
        let mut goal = self.camera;
        goal.frame_sphere(*pos, (self.bounds.1 * 0.22).max(0.3));
        self.camera_goal = Some(goal);
    }

    // ── Overlay UI ─────────────────────────────────────────────────────────

    fn view_toggle(
        &self,
        id: &'static str,
        icon: IconName,
        tooltip: &'static str,
        on: bool,
        flag: fn(&mut ViewOptions) -> &mut bool,
    ) -> Button {
        let editor = self.editor.clone();
        Button::new(id)
            .icon(icon)
            .ghost()
            .small()
            .selected(on)
            .tooltip(tooltip)
            .on_click(move |_, _, cx| {
                if let Some(editor) = editor.upgrade() {
                    editor.update(cx, |e, cx| {
                        let f = flag(&mut e.view_options);
                        *f = !*f;
                        cx.notify();
                    });
                }
            })
    }

    fn render_toolbar(
        &self,
        options: ViewOptions,
        tracers: bool,
        mesh_lod_count: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let editor = self.editor.clone();
        let lod_editor = editor.clone();

        h_flex()
            .absolute()
            .top_2()
            .left_2()
            .gap_0p5()
            .px_1()
            .py_1()
            .items_center()
            .rounded(px(8.0))
            .bg(theme.popover.opacity(0.88))
            .border_1()
            .border_color(theme.border)
            .shadow_md()
            .child(
                Button::new("vp-frame-all")
                    .icon(IconName::Maximize)
                    .ghost()
                    .small()
                    .tooltip("Frame everything (Home)")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.frame_all();
                        cx.notify();
                    })),
            )
            .child(
                Button::new("vp-frame-selected")
                    .icon(IconName::FrameSelect)
                    .ghost()
                    .small()
                    .tooltip("Frame the selected bone (F)")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.frame_selected(cx);
                        cx.notify();
                    })),
            )
            .child(toolbar_divider(cx))
            .child(self.view_toggle(
                "vp-grid",
                IconName::DotsGrid3x3,
                "Ground grid",
                options.grid,
                |o| &mut o.grid,
            ))
            .child(self.view_toggle(
                "vp-bones",
                IconName::Component,
                "Bone shapes",
                options.bones,
                |o| &mut o.bones,
            ))
            .child(self.view_toggle(
                "vp-joints",
                IconName::Circle,
                "Joint pivots",
                options.joints,
                |o| &mut o.joints,
            ))
            .child(self.view_toggle(
                "vp-physics",
                IconName::Circle,
                "Physics rig: show per-bone colliders and joint limits",
                options.physics,
                |o| &mut o.physics,
            ))
            .child(self.view_toggle(
                "vp-xray",
                IconName::Eye,
                "X-ray: show bones hidden behind others",
                options.xray,
                |o| &mut o.xray,
            ))
            .when(mesh_lod_count > 0, |bar| {
                bar.child(toolbar_divider(cx))
                    .child(self.view_toggle(
                        "vp-mesh",
                        IconName::Cube,
                        "Skeletal mesh: draw the skinned mesh",
                        options.mesh,
                        |o| &mut o.mesh,
                    ))
                    .child(self.view_toggle(
                        "vp-weights",
                        IconName::ColorFilter,
                        "Weights: color the mesh by the selected bone's influence",
                        options.weights,
                        |o| &mut o.weights,
                    ))
                    .child(
                        Button::new("vp-mesh-lod")
                            .label(format!(
                                "LOD {}",
                                options.mesh_lod.min(mesh_lod_count - 1) + 1
                            ))
                            .ghost()
                            .small()
                            .tooltip("Cycle the skeletal mesh viewport LOD")
                            .on_click(move |_, _, cx| {
                                if let Some(editor) = lod_editor.upgrade() {
                                    editor.update(cx, |e, cx| {
                                        e.view_options.mesh_lod =
                                            (e.view_options.mesh_lod + 1) % mesh_lod_count;
                                        cx.notify();
                                    });
                                }
                            }),
                    )
            })
            .child(toolbar_divider(cx))
            .child(self.view_toggle(
                "vp-onion",
                IconName::Copy,
                "Onion skin: ghost poses around the playhead",
                options.onion_skin,
                |o| &mut o.onion_skin,
            ))
            .child(self.view_toggle(
                "vp-path",
                IconName::PathArrow,
                "Motion path of the selected bone",
                options.motion_path,
                |o| &mut o.motion_path,
            ))
            .child(
                Button::new("vp-tracers")
                    .icon(IconName::Activity)
                    .ghost()
                    .small()
                    .selected(tracers)
                    .tooltip("Tracers: record every bone's trail during playback")
                    .on_click(move |_, _, cx| {
                        if let Some(editor) = editor.upgrade() {
                            editor.update(cx, |e, cx| e.toggle_tracers(cx));
                        }
                    }),
            )
    }

    /// Floating name tag next to the selected / hovered joint.
    fn bone_label(&self, id: &str, selected: bool, name: &str, cx: &App) -> Option<AnyElement> {
        let theme = cx.theme();
        let (_, pos) = self.joints.iter().find(|(b, _)| b == id)?;
        let (x, y) = self.project(*pos)?;
        if x < -40.0
            || y < -20.0
            || x > self.last_size.width + 40.0
            || y > self.last_size.height + 20.0
        {
            return None;
        }
        let accent = if selected { theme.warning } else { theme.info };
        Some(
            h_flex()
                .absolute()
                .left(px(x + 16.0))
                .top(px(y - 24.0))
                .gap_1()
                .px_1p5()
                .py_0p5()
                .items_center()
                .rounded(px(5.0))
                .bg(theme.popover.opacity(0.92))
                .border_1()
                .border_color(accent.opacity(0.7))
                .shadow_sm()
                .child(
                    Icon::new(IconName::Component)
                        .size(px(11.0))
                        .text_color(accent),
                )
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .text_color(theme.popover_foreground)
                        .child(name.to_string()),
                )
                .into_any_element(),
        )
    }

    fn render_hud(&self, editor: &SkeletalAnimEditorPanel, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let selected = editor
            .selected_bone
            .as_ref()
            .and_then(|id| editor.skeleton.bone(id));

        h_flex()
            .absolute()
            .left_2()
            .bottom_2()
            .gap_1p5()
            .items_center()
            .child(Tag::secondary().small().child("Perspective"))
            .when_some(selected, |row, bone| {
                let depth = editor
                    .skeleton
                    .depth_first()
                    .iter()
                    .find(|(b, _)| b.id == bone.id)
                    .map_or(0, |(_, d)| *d);
                row.child(Tag::primary().small().child(bone.name.clone()))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .font_family(MONO)
                            .text_color(theme.muted_foreground)
                            .child(format!("depth {depth}")),
                    )
            })
            .child(
                div()
                    .text_size(px(10.5))
                    .font_family(MONO)
                    .text_color(theme.muted_foreground)
                    .child(format!(
                        "{} bones · frame {}",
                        editor.skeleton.bones.len(),
                        editor.current_frame()
                    )),
            )
    }
}

impl EventEmitter<PanelEvent> for ViewportPanel {}

impl Focusable for ViewportPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ui::dock::Panel for ViewportPanel {
    fn panel_name(&self) -> &'static str {
        "skeletal-viewport"
    }

    fn title(&self, _window: &Window, _cx: &App) -> AnyElement {
        "Viewport".into_any_element()
    }
}

fn load_physics_asset(root: &std::path::Path, mesh_id: &str) -> Option<SkeletalPhysicsAsset> {
    let index = engine_fs::open_project_file_index(root).ok().flatten()?;
    let files = index
        .query(&engine_fs::FileIndexQuery {
            extension: Some("physasset".to_owned()),
            sort: engine_fs::FileIndexSort::Path,
            ..Default::default()
        })
        .ok()?;
    for file in files {
        let path = index
            .project_root()
            .join(file.path.replace('/', std::path::MAIN_SEPARATOR_STR));
        let Ok(bytes) = engine_fs::virtual_fs::read_file(&path) else {
            continue;
        };
        let Ok(asset) = serde_json::from_slice::<SkeletalPhysicsAsset>(&bytes) else {
            continue;
        };
        if asset.skeletal_mesh.as_str() == mesh_id {
            return Some(asset);
        }
    }
    None
}

impl Render for ViewportPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(editor) = self.editor.upgrade() else {
            return div().size_full().child("No editor").into_any_element();
        };

        self.palette = Palette::from_theme(cx.theme());
        let palette = self.palette;

        let physics_key = {
            let e = editor.read(cx);
            e.mesh_id
                .as_ref()
                .or(e.preview_mesh_ref.as_ref())
                .map(|id| (e.project_root.clone(), id.as_str().to_owned()))
        };
        if physics_key != self.physics_cache_key
            || self.physics_cache_checked.elapsed() >= Duration::from_secs(1)
        {
            self.physics_asset = physics_key
                .as_ref()
                .and_then(|(root, id)| load_physics_asset(root, id));
            self.physics_cache_key = physics_key;
            self.physics_cache_checked = Instant::now();
        }

        // Build the frame's geometry from the editor's current pose.
        let (geo, options, tracers_on, mesh_lod_count, hud, labels) = {
            let e = editor.read(cx);
            let (start, end) = e.play_range_seconds();
            let geo = build_scene(&SceneInput {
                skeleton: &e.skeleton,
                clip: &e.animation,
                time: e.playback.time,
                range: (start, end),
                selected: e.selected_bone.as_deref(),
                hovered: e.hovered_bone.as_deref(),
                view: e.view_options,
                palette: &palette,
                tracers: e
                    .debug
                    .show_tracers
                    .then_some((&e.tracer_state.trails, &e.tracer_state.colors)),
                skin: e
                    .mesh
                    .as_deref()
                    .filter(|_| e.view_options.mesh)
                    .map(|mesh| SkinInput {
                        mesh,
                        weights_of: e
                            .view_options
                            .weights
                            .then(|| e.selected_bone.as_deref())
                            .flatten(),
                    }),
                physics: self.physics_asset.as_ref(),
            });
            let name = |id: &str| {
                e.skeleton
                    .bone(id)
                    .map_or_else(|| id.to_string(), |b| b.name.clone())
            };
            let labels: Vec<(String, bool, String)> = [
                e.selected_bone
                    .as_deref()
                    .map(|id| (id.to_string(), true, name(id))),
                e.hovered_bone
                    .as_deref()
                    .filter(|h| e.selected_bone.as_deref() != Some(*h))
                    .map(|id| (id.to_string(), false, name(id))),
            ]
            .into_iter()
            .flatten()
            .collect();
            (
                geo,
                e.view_options,
                e.debug.show_tracers,
                e.mesh.as_deref().map_or(0, |mesh| mesh.lods.len()),
                self.render_hud(e, cx).into_any_element(),
                labels,
            )
        };

        self.joints = geo.joint_positions.clone();
        self.bounds = (geo.center, geo.radius.max(0.25));
        if self.needs_fit && geo.radius > 0.0 {
            self.camera.frame_sphere(geo.center, geo.radius);
            self.needs_fit = false;
        }
        let shadow = geo.shadow(&palette);
        self.gizmo_layout = build_gizmo(
            &self.camera,
            self.last_size.width.max(1.0),
            self.last_size.height.max(1.0),
            &palette,
        )
        .layout;

        let label_elements: Vec<AnyElement> = labels
            .iter()
            .filter_map(|(id, selected, name)| self.bone_label(id, *selected, name, cx))
            .collect();
        let toolbar = self.render_toolbar(options, tracers_on, mesh_lod_count, cx);

        let gpu_display: AnyElement = if let Some(ref s) = self.surface {
            wgpu_surface(s.clone())
                .defer_resize_until_mouse_up(true)
                .absolute()
                .inset_0()
                .into_any_element()
        } else {
            div()
                .absolute()
                .inset_0()
                .bg(cx.theme().background)
                .into_any_element()
        };

        let entity = cx.entity().clone();
        let driver = {
            let pre = entity.clone();
            let paint = entity.clone();
            gpui::canvas(
                move |bounds, window, cx| {
                    let sw = bounds.size.width.as_f32().max(1.0) as u32;
                    let sh = bounds.size.height.as_f32().max(1.0) as u32;
                    pre.update(cx, |panel, cx| {
                        panel.last_origin =
                            Point::new(bounds.origin.x.as_f32(), bounds.origin.y.as_f32());
                        panel.last_size =
                            Size::new(bounds.size.width.as_f32(), bounds.size.height.as_f32());
                        if panel.surface.is_none() {
                            if let Some(s) = window.create_wgpu_surface(
                                sw.max(64),
                                sh.max(64),
                                wgpu::TextureFormat::Bgra8UnormSrgb,
                            ) {
                                panel.surface = Some(s);
                                cx.notify();
                            }
                        }
                    });
                },
                move |_bounds, _pre, _window, cx| {
                    paint.update(cx, |panel, _cx| {
                        let Some(ref surface) = panel.surface else {
                            return;
                        };
                        if surface.is_resize_pending() {
                            return;
                        }
                        let Some((view, (w, h))) = surface.back_view_with_size() else {
                            return;
                        };

                        let aspect = w as f32 / h.max(1) as f32;
                        let view_proj = panel.camera.view_proj(aspect);
                        panel.last_view_proj = view_proj;
                        let eye = panel.camera.eye().to_array();
                        let uniforms = ViewportUniforms::new(
                            &palette,
                            view_proj,
                            eye,
                            [w as f32, h as f32],
                            (panel.camera.distance * 8.0).max(30.0),
                            shadow,
                        );
                        let gizmo = build_gizmo(&panel.camera, w as f32, h as f32, &palette);

                        let (device, queue, fmt) = (
                            surface.device().clone(),
                            surface.queue().clone(),
                            surface.format(),
                        );
                        panel.renderer.render_frame(
                            &device,
                            &queue,
                            &view,
                            w,
                            h,
                            fmt,
                            &SceneFrame {
                                uniforms: &uniforms,
                                mesh: &geo.mesh,
                                skin: &geo.skin,
                                ghost: &geo.ghost,
                                ribbons: &geo.ribbons,
                                joints: &geo.joints,
                                gizmo_ribbons: &gizmo.ribbons,
                                gizmo_bubbles: &gizmo.bubbles,
                                show_grid: options.grid,
                                xray: options.xray,
                            },
                        );
                        drop(view);
                        if let Some(ref surface) = panel.surface {
                            surface.swap_buffers();
                        }
                    });
                },
            )
            .absolute()
            .inset_0()
            .size_full()
        };

        div()
            .id("skeletal-viewport")
            .size_full()
            .relative()
            .overflow_hidden()
            .track_focus(&self.focus_handle)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, e: &MouseDownEvent, w, cx| this.handle_mouse_down(e, w, cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, e: &MouseDownEvent, w, cx| this.handle_mouse_down(e, w, cx)),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, e: &MouseDownEvent, w, cx| this.handle_mouse_down(e, w, cx)),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.handle_mouse_up()),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.handle_mouse_up()),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.handle_mouse_up()),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.handle_mouse_up()),
            )
            .on_mouse_up_out(
                MouseButton::Right,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.handle_mouse_up()),
            )
            .on_mouse_up_out(
                MouseButton::Middle,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.handle_mouse_up()),
            )
            .on_mouse_move(
                cx.listener(|this, e: &MouseMoveEvent, _, cx| this.handle_mouse_move(e, cx)),
            )
            .on_scroll_wheel(
                cx.listener(|this, e: &ScrollWheelEvent, _, cx| this.handle_scroll(e, cx)),
            )
            .on_key_down(
                cx.listener(|this, e: &KeyDownEvent, w, cx| this.handle_key_down(e, w, cx)),
            )
            .on_key_up(cx.listener(|this, e: &KeyUpEvent, _, cx| this.handle_key_up(e, cx)))
            .on_modifiers_changed(cx.listener(|this, e: &ModifiersChangedEvent, _, cx| {
                this.handle_modifiers_changed(e, cx)
            }))
            .child(gpu_display)
            .child(driver)
            .children(label_elements)
            .child(toolbar)
            .child(hud)
            .into_any_element()
    }
}
