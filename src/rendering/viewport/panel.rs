//! `ViewportPanel` — dock panel hosting the 3D bone viewport.
//!
//! Renders a ground grid, the animated skeleton (bone segments + joint
//! markers) and supports an orbit camera (drag to rotate, scroll to zoom,
//! shift-drag to pan) plus click-to-select on a joint.

use crate::core::{evaluate_world_transforms, Mat4, Vec3};
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
            target: Vec3::new(0.0, 1.0, 0.0),
            yaw_deg: 35.0,
            pitch_deg: 20.0,
            distance: 4.0,
        }
    }
}

pub struct ViewportPanel {
    editor: WeakEntity<SkeletalAnimEditorPanel>,
    focus_handle: FocusHandle,
    renderer: ViewportRenderer,
    surface: Option<WgpuSurfaceHandle>,
    camera: OrbitCamera,
    drag_last: Option<Point<f32>>,
    panning: bool,
    /// View-projection and screen bounds from the most recent paint, used to
    /// project joint positions for click-to-select.
    last_view_proj: Mat4,
    last_origin: Point<f32>,
    last_size: Size<f32>,
}

impl ViewportPanel {
    pub fn new(editor: WeakEntity<SkeletalAnimEditorPanel>, cx: &mut Context<Self>) -> Self {
        Self {
            editor,
            focus_handle: cx.focus_handle(),
            renderer: ViewportRenderer::new(),
            surface: None,
            camera: OrbitCamera::default(),
            drag_last: None,
            panning: false,
            last_view_proj: Mat4::IDENTITY,
            last_origin: Point::new(0.0, 0.0),
            last_size: Size::new(1.0, 1.0),
        }
    }

    fn handle_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.drag_last = Some(Point::new(
            event.position.x.as_f32(),
            event.position.y.as_f32(),
        ));
        self.panning = event.modifiers.shift || event.button == MouseButton::Right;
        if event.button == MouseButton::Left && !event.modifiers.shift {
            self.select_bone_at(event.position, window, cx);
        }
    }

    fn handle_mouse_up(&mut self) {
        self.drag_last = None;
        self.panning = false;
    }

    fn handle_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(last) = self.drag_last else { return };
        let pos = Point::new(event.position.x.as_f32(), event.position.y.as_f32());
        let delta = Point::new(pos.x - last.x, pos.y - last.y);
        self.drag_last = Some(pos);

        if self.panning {
            let yaw = self.camera.yaw_deg.to_radians();
            let right = Vec3::new(yaw.cos(), 0.0, -yaw.sin());
            let up = Vec3::new(0.0, 1.0, 0.0);
            let scale = self.camera.distance * 0.0015;
            self.camera.target = self
                .camera
                .target
                .add(right.scale(-delta.x * scale))
                .add(up.scale(delta.y * scale));
        } else {
            self.camera.yaw_deg -= delta.x * 0.4;
            self.camera.pitch_deg = (self.camera.pitch_deg + delta.y * 0.4).clamp(-89.0, 89.0);
        }
        cx.notify();
    }

    fn handle_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let delta = match event.delta {
            ScrollDelta::Lines(p) => p.y,
            ScrollDelta::Pixels(p) => p.y.as_f32() / 40.0,
        };
        self.camera.distance = (self.camera.distance * (1.0 - delta * 0.1)).clamp(0.5, 30.0);
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

        let entity_down = entity.clone();
        let entity_up = entity.clone();
        let entity_move = entity.clone();
        let entity_scroll = entity.clone();
        let entity_reset = entity.clone();

        let controls = div()
            .absolute()
            .top_2()
            .right_2()
            .child(
                Button::new("viewport-reset-camera")
                    .icon(IconName::Maximize)
                    .tooltip("Reset Camera")
                    .on_click(move |_, _window, cx| {
                        entity_reset.update(cx, |panel, cx| {
                            panel.camera = OrbitCamera::default();
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
                    entity_down.update(cx, |panel, cx| panel.handle_mouse_down(event, window, cx));
                },
            )
            .on_mouse_down(
                MouseButton::Right,
                move |event: &MouseDownEvent, window, cx| {
                    entity.update(cx, |panel, cx| panel.handle_mouse_down(event, window, cx));
                },
            )
            .on_mouse_up(
                MouseButton::Left,
                {
                    let entity_up = entity_up.clone();
                    move |_event: &MouseUpEvent, _window, cx| {
                        entity_up.update(cx, |panel, _cx| panel.handle_mouse_up());
                    }
                },
            )
            .on_mouse_up(
                MouseButton::Right,
                move |_event: &MouseUpEvent, _window, cx| {
                    entity_up.update(cx, |panel, _cx| panel.handle_mouse_up());
                },
            )
            .on_mouse_move(move |event: &MouseMoveEvent, _window, cx| {
                entity_move.update(cx, |panel, cx| panel.handle_mouse_move(event, cx));
            })
            .on_scroll_wheel(move |event: &ScrollWheelEvent, _window, cx| {
                entity_scroll.update(cx, |panel, cx| panel.handle_scroll(event, cx));
            })
            .child(gpu_display)
            .child(driver)
            .child(controls)
            .into_any_element()
    }
}
