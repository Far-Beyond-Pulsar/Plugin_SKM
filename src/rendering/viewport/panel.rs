//! `ViewportPanel` — dock panel hosting the 3D bone viewport.
//!
//! Renders a ground grid, the animated skeleton (bone segments + joint
//! markers) and supports Unreal Engine-style viewport controls:
//!
//! | Input                | Action                              |
//! |----------------------|-------------------------------------|
//! | RMB + drag           | Look (FPS-style, eye stays fixed)   |
//! | MMB + drag           | Pan (translate camera laterally)    |
//! | Alt + LMB + drag     | Orbit around target                 |
//! | Scroll wheel         | Zoom / dolly                        |
//! | LMB click            | Select bone                         |

use std::collections::HashMap;

use crate::core::{evaluate_world_transforms, Mat4, Skeleton, Vec3};
use crate::editor::panel::SkeletalAnimEditorPanel;
use gpui::*;
use ui::button::Button;
use ui::PixelsExt;
use ui::{dock::PanelEvent, ActiveTheme, IconName};

use super::renderer::ViewportRenderer;
use super::types::{JointInstance, LineVertex, ViewportUniforms};

const GRID_EXTENT: i32 = 8;
const GRID_COLOR: [f32; 4] = [0.30, 0.31, 0.34, 0.6];
const AXIS_X_COLOR: [f32; 4] = [0.75, 0.25, 0.25, 1.0];
const AXIS_Z_COLOR: [f32; 4] = [0.25, 0.35, 0.80, 1.0];
const BONE_COLOR: [f32; 4] = [0.85, 0.85, 0.88, 1.0];
const BONE_SELECTED_COLOR: [f32; 4] = [1.0, 0.65, 0.15, 1.0];
const JOINT_COLOR: [f32; 4] = [0.55, 0.75, 1.0, 1.0];
const JOINT_SELECTED_COLOR: [f32; 4] = [1.0, 0.65, 0.15, 1.0];
const JOINT_SIZE_PX: f32 = 10.0;

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

/// Orbit camera: yaw/pitch around a target point at a fixed distance.
pub struct OrbitCamera {
    pub target: Vec3,
    pub yaw_deg: f32,
    pub pitch_deg: f32,
    pub distance: f32,
}

impl OrbitCamera {
    fn eye(&self) -> Vec3 {
        let yaw = self.yaw_deg.to_radians();
        let pitch = self.pitch_deg.to_radians();
        let dir = Vec3::new(
            yaw.sin() * pitch.cos(),
            pitch.sin(),
            yaw.cos() * pitch.cos(),
        );
        self.target.add(dir.scale(self.distance))
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let view = Mat4::look_at(self.eye(), self.target, Vec3::new(0.0, 1.0, 0.0));
        let proj = Mat4::perspective(45.0, aspect.max(0.01), 0.05, 100.0);
        proj.mul(&view)
    }
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            // These are overwritten by `fit_camera_to_skeleton` on the first render;
            // they only apply if the skeleton is empty.
            target: Vec3::ZERO,
            yaw_deg: 45.0,
            pitch_deg: 20.0,
            distance: 3.0,
        }
    }
}

#[derive(Default)]
struct InputState {
    forward: bool,
    backward: bool,
    left: bool,
    right: bool,
    up: bool,
    down: bool,
}

pub struct ViewportPanel {
    editor: WeakEntity<SkeletalAnimEditorPanel>,
    focus_handle: FocusHandle,
    renderer: ViewportRenderer,
    surface: Option<WgpuSurfaceHandle>,
    camera: OrbitCamera,
    drag_last: Option<Point<f32>>,
    drag_mode: DragMode,
    input_state: InputState,
    /// True until the first render where we can read the skeleton and compute
    /// a camera distance/target that frames it exactly.
    needs_fit: bool,
    /// View-projection and screen bounds from the most recent paint, used to
    /// project joint positions for click-to-select.
    last_view_proj: Mat4,
    last_origin: Point<f32>,
    last_size: Size<f32>,
}

impl ViewportPanel {
    pub fn new(editor: WeakEntity<SkeletalAnimEditorPanel>, cx: &mut Context<Self>) -> Self {
        let panel = Self {
            editor,
            focus_handle: cx.focus_handle(),
            renderer: ViewportRenderer::new(),
            surface: None,
            camera: OrbitCamera::default(),
            drag_last: None,
            drag_mode: DragMode::None,
            input_state: InputState::default(),
            needs_fit: true,
            last_view_proj: Mat4::IDENTITY,
            last_origin: Point::new(0.0, 0.0),
            last_size: Size::new(1.0, 1.0),
        };

        let weak = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            const FRAME: std::time::Duration = std::time::Duration::from_millis(16);
            loop {
                smol::Timer::after(FRAME).await;
                let still_alive = weak.update(cx, |panel, cx| {
                    panel.tick_camera_movement(cx);
                    true
                });
                match still_alive {
                    Ok(true) => {}
                    _ => break,
                }
            }
        })
        .detach();

        panel
    }

    fn handle_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window, cx);

        match event.button {
            // RMB → FPS-style look (Unreal perspective drag)
            MouseButton::Right => {
                if event.modifiers.alt {
                    self.drag_mode = DragMode::Zoom;
                } else {
                    self.drag_mode = DragMode::Look;
                }
                self.drag_last = Some(Point::new(
                    event.position.x.as_f32(),
                    event.position.y.as_f32(),
                ));
            }
            // MMB → pan
            MouseButton::Middle => {
                self.drag_mode = DragMode::Pan;
                self.drag_last = Some(Point::new(
                    event.position.x.as_f32(),
                    event.position.y.as_f32(),
                ));
            }
            // LMB + Alt → orbit; plain LMB → select
            MouseButton::Left => {
                if event.modifiers.alt {
                    self.drag_mode = DragMode::Orbit;
                    self.drag_last = Some(Point::new(
                        event.position.x.as_f32(),
                        event.position.y.as_f32(),
                    ));
                } else {
                    self.select_bone_at(event.position, window, cx);
                }
            }
            _ => {}
        }
    }

    fn handle_mouse_up(&mut self) {
        self.drag_last = None;
        if self.drag_mode == DragMode::Look {
            // Reset input state when stopping fly mode.
            self.input_state = InputState::default();
        }
        self.drag_mode = DragMode::None;
    }

    fn handle_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if self.drag_mode != DragMode::Look {
            return;
        }
        let key = event.keystroke.key.as_str();
        let mut changed = true;
        match key {
            "w" | "W" => self.input_state.forward = true,
            "s" | "S" => self.input_state.backward = true,
            "a" | "A" => self.input_state.left = true,
            "d" | "D" => self.input_state.right = true,
            "space" | " " => self.input_state.up = true,
            "shift" | "shift_l" | "shift_r" => self.input_state.down = true,
            _ => changed = false,
        }
        if changed {
            cx.notify();
        }
    }

    fn handle_key_up(&mut self, event: &KeyUpEvent, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let mut changed = true;
        match key {
            "w" | "W" => self.input_state.forward = false,
            "s" | "S" => self.input_state.backward = false,
            "a" | "A" => self.input_state.left = false,
            "d" | "D" => self.input_state.right = false,
            "space" | " " => self.input_state.up = false,
            "shift" | "shift_l" | "shift_r" => self.input_state.down = false,
            _ => changed = false,
        }
        if changed {
            cx.notify();
        }
    }

    fn tick_camera_movement(&mut self, cx: &mut Context<Self>) {
        if self.drag_mode != DragMode::Look {
            return;
        }

        let mut movement = Vec3::ZERO;

        let yaw = self.camera.yaw_deg.to_radians();
        let pitch = self.camera.pitch_deg.to_radians();

        // The direction the camera is facing
        let forward = Vec3::new(
            yaw.sin() * pitch.cos(),
            pitch.sin(),
            yaw.cos() * pitch.cos(),
        );
        let world_up = Vec3::new(0.0, 1.0, 0.0);
        // "right" is the cross product of world up and forward
        let right = world_up.cross(forward).normalize();

        if self.input_state.forward {
            movement = movement.add(forward);
        }
        if self.input_state.backward {
            movement = movement.sub(forward);
        }
        if self.input_state.right {
            movement = movement.add(right);
        }
        if self.input_state.left {
            movement = movement.sub(right);
        }
        if self.input_state.up {
            movement = movement.add(world_up);
        }
        if self.input_state.down {
            movement = movement.sub(world_up);
        }

        if movement.length() > 0.001 {
            // Move speed proportional to distance makes it feel better at scale
            let speed = (self.camera.distance * 0.02).clamp(0.05, 1.0);
            movement = movement.normalize().scale(speed);
            self.camera.target = self.camera.target.add(movement);
            cx.notify();
        }
    }

    fn handle_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(last) = self.drag_last else { return };
        if self.drag_mode == DragMode::None {
            return;
        }

        let pos = Point::new(event.position.x.as_f32(), event.position.y.as_f32());
        let delta = Point::new(pos.x - last.x, pos.y - last.y);
        self.drag_last = Some(pos);

        match self.drag_mode {
            // FPS look: keep the camera eye fixed and swing the target.
            DragMode::Look => {
                let eye = self.camera.eye();
                self.camera.yaw_deg -= delta.x * 0.3;
                // Inverted pitch so dragging mouse down (positive delta.y) looks down (negative pitch)
                self.camera.pitch_deg = (self.camera.pitch_deg + delta.y * 0.3).clamp(-89.0, 89.0);

                let yaw = self.camera.yaw_deg.to_radians();
                let pitch = self.camera.pitch_deg.to_radians();
                let dir = Vec3::new(
                    yaw.sin() * pitch.cos(),
                    pitch.sin(),
                    yaw.cos() * pitch.cos(),
                );

                // eye = target + dir * distance  =>  target = eye - dir * distance
                self.camera.target = eye.sub(dir.scale(self.camera.distance));
            }
            // Pan: translate both eye and target in screen space.
            DragMode::Pan => {
                let yaw = self.camera.yaw_deg.to_radians();
                let pitch = self.camera.pitch_deg.to_radians();

                // direction from target to eye
                let dir = Vec3::new(
                    yaw.sin() * pitch.cos(),
                    pitch.sin(),
                    yaw.cos() * pitch.cos(),
                );
                // direction from eye to target (forward)
                let look = dir.scale(-1.0);
                let world_up = Vec3::new(0.0, 1.0, 0.0);

                // screen right
                let right = look.cross(world_up).normalize();
                // screen up
                let up = right.cross(look).normalize();

                let scale = self.camera.distance * 0.0015;
                self.camera.target = self
                    .camera
                    .target
                    .add(right.scale(-delta.x * scale))
                    .add(up.scale(delta.y * scale));
            }
            // Orbit: classic eye-around-target rotation.
            DragMode::Orbit => {
                self.camera.yaw_deg -= delta.x * 0.4;
                // Inverted pitch
                self.camera.pitch_deg = (self.camera.pitch_deg - delta.y * 0.4).clamp(-89.0, 89.0);
            }
            // Zoom: change distance to target
            DragMode::Zoom => {
                let scale = self.camera.distance * 0.005;
                self.camera.distance = (self.camera.distance + delta.y * scale).clamp(0.1, 100.0);
            }
            DragMode::None => {}
        }
        cx.notify();
    }

    fn handle_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let delta = match event.delta {
            ScrollDelta::Lines(p) => p.y,
            ScrollDelta::Pixels(p) => p.y.as_f32() / 40.0,
        };
        self.camera.distance = (self.camera.distance * (1.0 - delta * 0.1)).clamp(0.1, 100.0);
        cx.notify();
    }

    /// Find the joint nearest to `pos` in screen space and select its bone.
    fn select_bone_at(&mut self, pos: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.upgrade() else {
            return;
        };
        let w = self.last_size.width.max(1.0);
        let h = self.last_size.height.max(1.0);
        let click = Point::new(
            pos.x.as_f32() - self.last_origin.x,
            pos.y.as_f32() - self.last_origin.y,
        );
        let view_proj = self.last_view_proj;

        editor.update(cx, |editor, cx| {
            let world = evaluate_world_transforms(
                &editor.skeleton,
                &editor.animation,
                editor.playback.time,
            );

            let mut best: Option<(String, f32)> = None;
            for bone in &editor.skeleton.bones {
                let Some(m) = world.get(&bone.id) else {
                    continue;
                };
                let p = m.transform_point(Vec3::ZERO);
                let (cx_, cy, _, cw) = view_proj.transform_clip(p);
                if cw <= 0.0 {
                    continue;
                }
                let sx = (cx_ / cw * 0.5 + 0.5) * w;
                let sy = (1.0 - (cy / cw * 0.5 + 0.5)) * h;
                let dx = sx - click.x;
                let dy = sy - click.y;
                let dist = (dx * dx + dy * dy).sqrt();
                if dist < JOINT_SIZE_PX * 1.5 && best.as_ref().map_or(true, |(_, d)| dist < *d) {
                    best = Some((bone.id.clone(), dist));
                }
            }

            if let Some((bone_id, _)) = best {
                editor.select_bone(Some(bone_id), window, cx);
            }
        });
    }

    /// Compute the bind-pose bounding sphere of `skeleton` and position the
    /// camera so the sphere just fills the vertical FOV.
    fn fit_camera_to_skeleton(&mut self, skeleton: &Skeleton) {
        if skeleton.bones.is_empty() {
            return;
        }

        // Walk bones in depth-first order, accumulating world matrices from
        // the bind pose only (no animation clip needed).
        let mut world: HashMap<String, Mat4> = HashMap::with_capacity(skeleton.bones.len());
        let mut positions: Vec<Vec3> = Vec::with_capacity(skeleton.bones.len());

        for (bone, _) in skeleton.depth_first() {
            let local = bone.bind_transform.to_matrix();
            let parent_world = bone
                .parent
                .as_deref()
                .and_then(|p| world.get(p))
                .copied()
                .unwrap_or(Mat4::IDENTITY);
            let mat = parent_world.mul(&local);
            positions.push(mat.transform_point(Vec3::ZERO));
            world.insert(bone.id.clone(), mat);
        }

        // AABB of all joint origins.
        let mut min = positions[0];
        let mut max = positions[0];
        for p in &positions[1..] {
            min = Vec3::new(min.x.min(p.x), min.y.min(p.y), min.z.min(p.z));
            max = Vec3::new(max.x.max(p.x), max.y.max(p.y), max.z.max(p.z));
        }

        let center = Vec3::new(
            (min.x + max.x) * 0.5,
            (min.y + max.y) * 0.5,
            (min.z + max.z) * 0.5,
        );

        // Bounding-sphere radius: furthest joint from center.
        let radius = positions
            .iter()
            .map(|p| p.sub(center).length())
            .fold(0.0f32, f32::max)
            .max(0.1); // guard against a degenerate single-joint rig at the origin

        // view_proj uses a 45° vertical FOV.  distance = r / tan(fov/2) puts the
        // sphere edge exactly at the frame edge; ×1.2 adds a comfortable padding.
        let half_fov = 22.5_f32.to_radians();
        self.camera.target = center;
        self.camera.distance = (radius / half_fov.tan() * 1.2).max(0.5);
    }

    /// Build the line and joint instance buffers for the current pose.
    fn build_scene(
        &self,
        editor: &SkeletalAnimEditorPanel,
    ) -> (Vec<LineVertex>, Vec<JointInstance>) {
        let mut lines = Vec::new();
        let mut joints = Vec::new();

        // Ground grid on the XZ plane.
        for i in -GRID_EXTENT..=GRID_EXTENT {
            let f = i as f32;
            let color = if i == 0 { AXIS_Z_COLOR } else { GRID_COLOR };
            lines.push(LineVertex {
                pos: [f, 0.0, -GRID_EXTENT as f32],
                color,
            });
            lines.push(LineVertex {
                pos: [f, 0.0, GRID_EXTENT as f32],
                color,
            });

            let color = if i == 0 { AXIS_X_COLOR } else { GRID_COLOR };
            lines.push(LineVertex {
                pos: [-GRID_EXTENT as f32, 0.0, f],
                color,
            });
            lines.push(LineVertex {
                pos: [GRID_EXTENT as f32, 0.0, f],
                color,
            });
        }

        let world =
            evaluate_world_transforms(&editor.skeleton, &editor.animation, editor.playback.time);
        let selected = editor.selected_bone.as_deref();

        for bone in &editor.skeleton.bones {
            let Some(m) = world.get(&bone.id) else {
                continue;
            };
            let pos = m.transform_point(Vec3::ZERO);
            let is_selected = selected == Some(bone.id.as_str());

            if let Some(parent_id) = &bone.parent {
                if let Some(pm) = world.get(parent_id) {
                    let ppos = pm.transform_point(Vec3::ZERO);
                    let color = if is_selected {
                        BONE_SELECTED_COLOR
                    } else {
                        BONE_COLOR
                    };
                    lines.push(LineVertex {
                        pos: ppos.to_array(),
                        color,
                    });
                    lines.push(LineVertex {
                        pos: pos.to_array(),
                        color,
                    });
                }
            }

            joints.push(JointInstance {
                center: pos.to_array(),
                size: JOINT_SIZE_PX,
                color: if is_selected {
                    JOINT_SELECTED_COLOR
                } else {
                    JOINT_COLOR
                },
            });
        }

        (lines, joints)
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

impl Render for ViewportPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(editor) = self.editor.upgrade() else {
            return div().size_full().child("No editor").into_any_element();
        };

        // On the first render (or after a reset), fit the camera to the skeleton.
        if self.needs_fit {
            let skeleton = editor.read(cx).skeleton.clone();
            self.fit_camera_to_skeleton(&skeleton);
            self.needs_fit = false;
        }

        let (lines, joints) = {
            let editor_ref = editor.read(cx);
            self.build_scene(editor_ref)
        };

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
                    paint.update(cx, |panel, cx| {
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
                        let uniforms = ViewportUniforms {
                            view_proj: view_proj.0,
                            viewport: [w as f32, h as f32],
                            time: 0.0,
                            _pad: 0.0,
                        };

                        panel.renderer.render_frame(
                            surface.device(),
                            surface.queue(),
                            &view,
                            w,
                            h,
                            surface.format(),
                            &uniforms,
                            &lines,
                            &joints,
                        );
                        drop(view);
                        surface.swap_buffers();
                        let _ = cx;
                    });
                },
            )
            .absolute()
            .inset_0()
            .size_full()
        };

        let entity_lmb_down = entity.clone();
        let entity_rmb_down = entity.clone();
        let entity_mmb_down = entity.clone();
        let entity_lmb_up = entity.clone();
        let entity_rmb_up = entity.clone();
        let entity_mmb_up = entity.clone();
        let entity_move = entity.clone();
        let entity_scroll = entity.clone();
        let entity_reset = entity.clone();
        let entity_key_down = entity.clone();
        let entity_key_up = entity.clone();

        let controls = div().absolute().top_2().right_2().child(
            Button::new("viewport-reset-camera")
                .icon(IconName::Maximize)
                .tooltip(
                    "Reset Camera (RMB: Look  |  MMB: Pan  |  Alt+LMB: Orbit  |  Scroll: Zoom)",
                )
                .on_click(move |_, _window, cx| {
                    entity_reset.update(cx, |panel, cx| {
                        // Re-fit to the current skeleton on the next render.
                        panel.camera.yaw_deg = 45.0;
                        panel.camera.pitch_deg = 20.0;
                        panel.needs_fit = true;
                        cx.notify();
                    });
                }),
        );

        div()
            .id("skeletal-viewport")
            .size_full()
            .relative()
            .overflow_hidden()
            .track_focus(&self.focus_handle)
            .on_mouse_down(
                MouseButton::Left,
                move |event: &MouseDownEvent, window, cx| {
                    entity_lmb_down
                        .update(cx, |panel, cx| panel.handle_mouse_down(event, window, cx));
                },
            )
            .on_mouse_down(
                MouseButton::Right,
                move |event: &MouseDownEvent, window, cx| {
                    entity_rmb_down
                        .update(cx, |panel, cx| panel.handle_mouse_down(event, window, cx));
                },
            )
            .on_mouse_down(
                MouseButton::Middle,
                move |event: &MouseDownEvent, window, cx| {
                    entity_mmb_down
                        .update(cx, |panel, cx| panel.handle_mouse_down(event, window, cx));
                },
            )
            .on_mouse_up(
                MouseButton::Left,
                move |_event: &MouseUpEvent, _window, cx| {
                    entity_lmb_up.update(cx, |panel, _cx| panel.handle_mouse_up());
                },
            )
            .on_mouse_up(
                MouseButton::Right,
                move |_event: &MouseUpEvent, _window, cx| {
                    entity_rmb_up.update(cx, |panel, _cx| panel.handle_mouse_up());
                },
            )
            .on_mouse_up(
                MouseButton::Middle,
                move |_event: &MouseUpEvent, _window, cx| {
                    entity_mmb_up.update(cx, |panel, _cx| panel.handle_mouse_up());
                },
            )
            .on_mouse_move(move |event: &MouseMoveEvent, _window, cx| {
                entity_move.update(cx, |panel, cx| panel.handle_mouse_move(event, cx));
            })
            .on_scroll_wheel(move |event: &ScrollWheelEvent, _window, cx| {
                entity_scroll.update(cx, |panel, cx| panel.handle_scroll(event, cx));
            })
            .on_key_down(move |event: &KeyDownEvent, _window, cx| {
                entity_key_down.update(cx, |panel, cx| panel.handle_key_down(event, cx));
            })
            .on_key_up(move |event: &KeyUpEvent, _window, cx| {
                entity_key_up.update(cx, |panel, cx| panel.handle_key_up(event, cx));
            })
            .child(gpu_display)
            .child(driver)
            .child(controls)
            .into_any_element()
    }
}
