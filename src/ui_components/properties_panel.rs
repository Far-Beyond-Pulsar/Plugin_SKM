//! Right dock panel: details and transform properties for the selected bone
//! or keyframe.

use std::collections::HashSet;

use gpui::prelude::FluentBuilder;
use gpui::*;
use ui::button::{Button, ButtonVariants};
use ui::dock::PanelEvent;
use ui::input::{InputState, NumberInput};
use ui::scroll::ScrollbarAxis;
use ui::tag::Tag;
use ui::{h_flex, v_flex, ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt};

use crate::editor::panel::SkeletalAnimEditorPanel;
use crate::editor::transform_inputs::TransformGroup;
use crate::style::{AXIS_X, AXIS_Y, AXIS_Z};
use crate::ui_components::chrome::{empty_message, panel_header, MONO};

pub struct BonePropertiesPanel {
    editor: WeakEntity<SkeletalAnimEditorPanel>,
    focus_handle: FocusHandle,
    collapsed: HashSet<&'static str>,
}

/// Snapshot of what the panel shows, read once per render.
struct Details {
    id: String,
    name: String,
    parent: Option<String>,
    child_count: usize,
    key_count: Option<usize>,
    /// Frame of the selected keyframe, if one is selected.
    selected_key: Option<(usize, i32, f32)>,
    keyed: bool,
    /// Animated bone with the playhead between keys: edits need a key first.
    locked: bool,
    /// Where the playhead sits relative to the bone's keys.
    timing: String,
    world: Option<[f32; 3]>,
    translation: [Entity<InputState>; 3],
    rotation: [Entity<InputState>; 3],
    scale: [Entity<InputState>; 3],
}

impl BonePropertiesPanel {
    pub fn new(editor: WeakEntity<SkeletalAnimEditorPanel>, cx: &mut Context<Self>) -> Self {
        Self {
            editor,
            focus_handle: cx.focus_handle(),
            collapsed: HashSet::new(),
        }
    }

    fn details(&self, cx: &App) -> Option<Details> {
        let editor = self.editor.upgrade()?;
        let e = editor.read(cx);
        let id = e.selected_bone.clone()?;
        let bone = e.skeleton.bone(&id)?;
        let track = e.animation.track(&id);
        let keyed = e.selected_keyframe.is_some();

        let selected_key = e.selected_keyframe.as_ref().and_then(|(bone_id, index)| {
            let kf = e.animation.track(bone_id)?.keyframes.get(*index)?;
            Some((index + 1, e.animation.time_to_frame(kf.time), kf.time))
        });

        let timing = match track {
            None => "Not animated — edits change the bind pose".to_string(),
            Some(t) if t.keyframes.is_empty() => "No keys".to_string(),
            Some(t) => {
                let time = e.playback.time;
                let frame = |s: f32| e.animation.time_to_frame(s);
                match e.keyframe_at_playhead(&id) {
                    Some(_) => format!("Keyed at frame {}", e.current_frame()),
                    None => {
                        let prev = t.keyframes.iter().rev().find(|k| k.time < time);
                        let next = t.keyframes.iter().find(|k| k.time > time);
                        match (prev, next) {
                            (Some(a), Some(b)) => {
                                format!("Interpolating frames {} → {}", frame(a.time), frame(b.time))
                            }
                            (Some(a), None) => format!("Holding last key (frame {})", frame(a.time)),
                            (None, Some(b)) => format!("Holding first key (frame {})", frame(b.time)),
                            (None, None) => "No keys".to_string(),
                        }
                    }
                }
            }
        };

        Some(Details {
            parent: bone
                .parent
                .as_ref()
                .and_then(|p| e.skeleton.bone(p))
                .map(|p| p.name.clone()),
            child_count: e.skeleton.children_of(&id).len(),
            key_count: track.map(|t| t.keyframes.len()),
            selected_key,
            keyed,
            locked: track.is_some() && !keyed,
            timing,
            world: e.world_position(&id).map(|p| [p.x, p.y, p.z]),
            translation: e.transform_inputs.translation.clone(),
            rotation: e.transform_inputs.rotation.clone(),
            scale: e.transform_inputs.scale.clone(),
            name: bone.name.clone(),
            id,
        })
    }

    fn chip(&self, icon: IconName, text: String, color: Hsla, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .gap_1()
            .px_1p5()
            .py_0p5()
            .items_center()
            .rounded(px(5.0))
            .bg(color.opacity(0.14))
            .border_1()
            .border_color(color.opacity(0.35))
            .child(Icon::new(icon).size(px(11.0)).text_color(color))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.foreground.opacity(0.9))
                    .child(text),
            )
    }

    fn render_bone_card(&self, d: &Details, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let location = match &d.parent {
            Some(parent) => self
                .chip(IconName::ArrowUp, format!("Parent: {parent}"), theme.info, cx)
                .into_any_element(),
            None => self
                .chip(IconName::Home, "Root".to_string(), theme.warning, cx)
                .into_any_element(),
        };
        let animation = match d.key_count {
            Some(n) => self
                .chip(IconName::Keyframes, format!("{n} keys"), theme.success, cx)
                .into_any_element(),
            None => self
                .chip(IconName::Minus, "Static".to_string(), theme.muted_foreground, cx)
                .into_any_element(),
        };

        v_flex()
            .w_full()
            .p_3()
            .gap_2()
            .border_b_1()
            .border_color(theme.border.opacity(0.6))
            .child(
                v_flex()
                    .gap_0p5()
                    .child(
                        div()
                            .text_base()
                            .font_semibold()
                            .text_color(theme.foreground)
                            .child(d.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family(MONO)
                            .text_color(theme.muted_foreground)
                            .child(d.id.clone()),
                    ),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_1()
                    .child(location)
                    .when(d.child_count > 0, |row| {
                        row.child(self.chip(
                            IconName::Component,
                            format!(
                                "{} {}",
                                d.child_count,
                                if d.child_count == 1 { "child" } else { "children" }
                            ),
                            theme.muted_foreground,
                            cx,
                        ))
                    })
                    .child(animation),
            )
    }

    /// Previous key / key-at-playhead toggle / next key, with a status line.
    fn render_key_card(&self, d: &Details, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let keyed_here = d.keyed;
        let (e_prev, e_next, e_toggle) = (self.editor.clone(), self.editor.clone(), self.editor.clone());

        v_flex()
            .w_full()
            .px_3()
            .py_2p5()
            .gap_2()
            .border_b_1()
            .border_color(theme.border.opacity(0.6))
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        Button::new("props-prev-key")
                            .icon(IconName::FastArrowLeft)
                            .ghost()
                            .small()
                            .tooltip("Previous keyframe")
                            .on_click(move |_, window, cx| {
                                let _ = e_prev.update(cx, |e, cx| e.goto_prev_keyframe(window, cx));
                            }),
                    )
                    .child(
                        div().flex_1().child(
                            Button::new("props-toggle-key")
                                .icon(if keyed_here {
                                    IconName::Keyframe
                                } else {
                                    IconName::KeyframePlus
                                })
                                .label(if keyed_here { "Remove key" } else { "Add key" })
                                .small()
                                .w_full()
                                .when(keyed_here, |b| b.danger())
                                .tooltip(if keyed_here {
                                    "Remove the keyframe at the playhead"
                                } else {
                                    "Capture the current pose as a keyframe (K)"
                                })
                                .on_click(move |_, window, cx| {
                                    let _ = e_toggle.update(cx, |e, cx| {
                                        if keyed_here {
                                            e.delete_keyframe_at_playhead(window, cx);
                                        } else {
                                            e.insert_keyframe(window, cx);
                                        }
                                    });
                                }),
                        ),
                    )
                    .child(
                        Button::new("props-next-key")
                            .icon(IconName::FastArrowRight)
                            .ghost()
                            .small()
                            .tooltip("Next keyframe")
                            .on_click(move |_, window, cx| {
                                let _ = e_next.update(cx, |e, cx| e.goto_next_keyframe(window, cx));
                            }),
                    ),
            )
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        Icon::new(if keyed_here {
                            IconName::Keyframe
                        } else {
                            IconName::Info
                        })
                        .size(px(12.0))
                        .text_color(if keyed_here {
                            theme.warning
                        } else {
                            theme.muted_foreground
                        }),
                    )
                    .child(div().flex_1().text_xs().text_color(theme.muted_foreground).child(
                        match d.selected_key {
                            Some((n, frame, time)) => {
                                format!("Key #{n} · frame {frame} · {time:.3}s")
                            }
                            None => d.timing.clone(),
                        },
                    )),
            )
    }

    fn axis_row(
        &self,
        axis: &'static str,
        color: Hsla,
        input: &Entity<InputState>,
        unit: &'static str,
        locked: bool,
        cx: &App,
    ) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .w_full()
            .gap_1p5()
            .items_center()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(20.0))
                    .h(px(24.0))
                    .rounded(px(4.0))
                    .bg(color.opacity(0.18))
                    .border_1()
                    .border_color(color.opacity(0.45))
                    .text_xs()
                    .font_semibold()
                    .font_family(MONO)
                    .text_color(color)
                    .child(axis),
            )
            .child(
                div().flex_1().child(
                    NumberInput::new(input).small().disabled(locked).suffix(
                        div()
                            .pr_1()
                            .text_xs()
                            .font_family(MONO)
                            .text_color(theme.muted_foreground)
                            .child(unit),
                    ),
                ),
            )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_section(
        &self,
        group: TransformGroup,
        title: &'static str,
        icon: IconName,
        unit: &'static str,
        inputs: &[Entity<InputState>; 3],
        locked: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let collapsed = self.collapsed.contains(title);
        let editor = self.editor.clone();

        v_flex()
            .w_full()
            .border_b_1()
            .border_color(theme.border.opacity(0.6))
            .child(
                h_flex()
                    .id(SharedString::from(format!("props-section-{title}")))
                    .w_full()
                    .h(px(30.0))
                    .px_2()
                    .gap_1p5()
                    .items_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.foreground.opacity(0.04)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.collapsed.remove(title) {
                            this.collapsed.insert(title);
                        }
                        cx.notify();
                    }))
                    .child(
                        Icon::new(if collapsed {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .size(px(12.0))
                        .text_color(theme.muted_foreground),
                    )
                    .child(Icon::new(icon).size(px(13.0)).text_color(theme.info))
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .font_semibold()
                            .text_color(theme.foreground)
                            .child(title),
                    )
                    .child(
                        Button::new(SharedString::from(format!("props-reset-{title}")))
                            .icon(IconName::Undo)
                            .ghost()
                            .xsmall()
                            .disabled(locked)
                            .stop_propagation(true)
                            .tooltip(format!("Reset {}", title.to_lowercase()))
                            .on_click(move |_, window, cx| {
                                let _ = editor.update(cx, |e, cx| e.reset_transform_group(group, window, cx));
                            }),
                    ),
            )
            .when(!collapsed, |section| {
                section.child(
                    v_flex()
                        .w_full()
                        .px_3()
                        .pb_2p5()
                        .gap_1p5()
                        .child(self.axis_row("X", AXIS_X, &inputs[0], unit, locked, cx))
                        .child(self.axis_row("Y", AXIS_Y, &inputs[1], unit, locked, cx))
                        .child(self.axis_row("Z", AXIS_Z, &inputs[2], unit, locked, cx)),
                )
            })
    }

    fn render_world(&self, world: [f32; 3], cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let cell = |axis: &'static str, color: Hsla, value: f32| {
            h_flex()
                .flex_1()
                .gap_1()
                .items_center()
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .font_family(MONO)
                        .text_color(color)
                        .child(axis),
                )
                .child(
                    div()
                        .text_xs()
                        .font_family(MONO)
                        .text_color(theme.foreground.opacity(0.85))
                        .child(format!("{value:.3}")),
                )
        };
        v_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_1()
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        Icon::new(IconName::Globe)
                            .size(px(12.0))
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(theme.muted_foreground)
                            .child("World position"),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(cell("X", AXIS_X, world[0]))
                    .child(cell("Y", AXIS_Y, world[1]))
                    .child(cell("Z", AXIS_Z, world[2])),
            )
    }
}

impl EventEmitter<PanelEvent> for BonePropertiesPanel {}

impl Focusable for BonePropertiesPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ui::dock::Panel for BonePropertiesPanel {
    fn panel_name(&self) -> &'static str {
        "skeletal-bone-properties"
    }

    fn title(&self, _window: &Window, _cx: &App) -> AnyElement {
        "Properties".into_any_element()
    }
}

impl Render for BonePropertiesPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let details = self.details(cx);

        let badge = match &details {
            Some(d) if d.selected_key.is_some() => Some(Tag::warning().small().child("Keyframe")),
            Some(_) => Some(Tag::info().small().child("Bone")),
            None => None,
        };
        let header = panel_header(
            IconName::Settings,
            "Details",
            badge.map(|b| b.into_any_element()),
            cx,
        );

        let body: AnyElement = match details {
            None => div()
                .flex_1()
                .min_h_0()
                .child(empty_message(
                    IconName::Component,
                    "Nothing selected",
                    "Pick a bone in the hierarchy or click a joint in the viewport",
                    cx,
                ))
                .into_any_element(),
            Some(d) => v_flex()
                .id("properties-scroll")
                .flex_1()
                .min_h_0()
                .scrollable(ScrollbarAxis::Vertical)
                .child(self.render_bone_card(&d, cx))
                .child(self.render_key_card(&d, cx))
                .when(d.locked, |col| {
                    col.child(
                        h_flex()
                            .w_full()
                            .mx_3()
                            .mt_2()
                            .px_2()
                            .py_1p5()
                            .gap_1p5()
                            .items_center()
                            .rounded(px(6.0))
                            .bg(theme.warning.opacity(0.10))
                            .border_1()
                            .border_color(theme.warning.opacity(0.35))
                            .child(Icon::new(IconName::Info).size(px(13.0)).text_color(theme.warning))
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .text_color(theme.foreground.opacity(0.9))
                                    .child("Add a key at the playhead to edit this pose."),
                            ),
                    )
                })
                .child(self.render_section(
                    TransformGroup::Translation,
                    "Translation",
                    IconName::Home,
                    "m",
                    &d.translation,
                    d.locked,
                    cx,
                ))
                .child(self.render_section(
                    TransformGroup::Rotation,
                    "Rotation",
                    IconName::RotateCameraLeft,
                    "°",
                    &d.rotation,
                    d.locked,
                    cx,
                ))
                .child(self.render_section(
                    TransformGroup::Scale,
                    "Scale",
                    IconName::Expand,
                    "×",
                    &d.scale,
                    d.locked,
                    cx,
                ))
                .when_some(d.world, |col, world| col.child(self.render_world(world, cx)))
                .into_any_element(),
        };

        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .child(header)
            .child(body)
            .into_any_element()
    }
}
