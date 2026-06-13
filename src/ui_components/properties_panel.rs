//! Right dock panel: transform properties for the selected bone or keyframe.

use gpui::prelude::FluentBuilder;
use gpui::*;
use ui::button::{Button, ButtonVariants};
use ui::dock::PanelEvent;
use ui::input::NumberInput;
use ui::{h_flex, v_flex, ActiveTheme, Disableable, Icon, IconName, Sizable};

use crate::editor::SkeletalAnimEditorPanel;

pub struct BonePropertiesPanel {
    editor: WeakEntity<SkeletalAnimEditorPanel>,
    focus_handle: FocusHandle,
}

impl BonePropertiesPanel {
    pub fn new(editor: WeakEntity<SkeletalAnimEditorPanel>, cx: &mut Context<Self>) -> Self {
        Self {
            editor,
            focus_handle: cx.focus_handle(),
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self
            .editor
            .upgrade()
            .and_then(|editor| {
                let editor = editor.read(cx);
                let bone = editor
                    .selected_bone
                    .as_ref()
                    .and_then(|id| editor.skeleton.bone(id));
                bone.map(|bone| bone.name.clone())
            })
            .unwrap_or_else(|| "No Selection".to_string());

        let subtitle = self.editor.upgrade().and_then(|editor| {
            let editor = editor.read(cx);
            editor
                .selected_keyframe
                .as_ref()
                .map(|(_, index)| format!("Keyframe #{}", index + 1))
        });

        v_flex()
            .w_full()
            .child(
                h_flex()
                    .w_full()
                    .px_2()
                    .py_1p5()
                    .bg(cx.theme().secondary)
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .items_center()
                    .gap_2()
                    .child(
                        Icon::new(IconName::Settings)
                            .size(px(16.0))
                            .text_color(cx.theme().info),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().foreground)
                            .child("Details"),
                    ),
            )
            .child(
                v_flex()
                    .w_full()
                    .px_2()
                    .py_1()
                    .gap_0p5()
                    .border_b_1()
                    .border_color(cx.theme().border.opacity(0.2))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(cx.theme().foreground)
                            .child(title),
                    )
                    .when_some(subtitle, |el, subtitle| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(subtitle),
                        )
                    }),
            )
    }

    /// A single X/Y/Z row: a label, the value input, and a "Key" icon button.
    ///
    /// The icon reflects whether the selected bone has a keyframe at the
    /// current playhead time (filled diamond = keyed, hollow = not keyed).
    /// Clicking it inserts a keyframe capturing the current pose (if not
    /// keyed) or removes the keyframe at the playhead (if keyed); the value
    /// inputs are only editable once keyed (or for non-animated bones, which
    /// edit the bind pose directly).
    fn render_axis_row(
        &self,
        section: &'static str,
        label: &'static str,
        input: &Entity<ui::input::InputState>,
        keyed: bool,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let editor = self.editor.clone();
        let icon = if keyed {
            IconName::Keyframe
        } else {
            IconName::KeyframePlus
        };
        let tooltip = if keyed {
            "Remove keyframe at playhead"
        } else {
            "Insert keyframe at playhead"
        };

        h_flex()
            .w_full()
            .gap_2()
            .items_center()
            .child(
                div()
                    .w(px(16.0))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(
                div()
                    .flex_1()
                    .child(NumberInput::new(input).w_full().disabled(disabled)),
            )
            .child(
                Button::new(SharedString::from(format!("key-{}-{}", section, label)))
                    .icon(icon)
                    .ghost()
                    .xsmall()
                    .tooltip(tooltip)
                    .on_click(move |_, window, cx| {
                        let Some(editor) = editor.upgrade() else {
                            return;
                        };
                        editor.update(cx, |editor, cx| {
                            if keyed {
                                editor.delete_keyframe_at_playhead(window, cx);
                            } else {
                                editor.insert_keyframe(window, cx);
                            }
                        });
                    }),
            )
    }

    fn render_section(
        &self,
        title: &'static str,
        inputs: &[Entity<ui::input::InputState>; 3],
        keyed: bool,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .w_full()
            .px_2()
            .py_2()
            .gap_2()
            .border_b_1()
            .border_color(cx.theme().border.opacity(0.2))
            .child(
                div()
                    .text_xs()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(cx.theme().muted_foreground)
                    .child(title),
            )
            .child(self.render_axis_row(title, "X", &inputs[0], keyed, disabled, cx))
            .child(self.render_axis_row(title, "Y", &inputs[1], keyed, disabled, cx))
            .child(self.render_axis_row(title, "Z", &inputs[2], keyed, disabled, cx))
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
        let Some(editor) = self.editor.upgrade() else {
            return v_flex().size_full().bg(cx.theme().sidebar);
        };

        let has_selection = editor.read(cx).selected_bone.is_some();
        let (translation, rotation, scale, keyed, disabled) = {
            let editor = editor.read(cx);
            let keyed = editor.selected_keyframe.is_some();
            let has_track = editor
                .selected_bone
                .as_ref()
                .is_some_and(|id| editor.animation.track(id).is_some());
            (
                editor.transform_inputs.translation.clone(),
                editor.transform_inputs.rotation.clone(),
                editor.transform_inputs.scale.clone(),
                keyed,
                has_track && !keyed,
            )
        };

        v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .child(self.render_header(cx))
            .when(has_selection, |el| {
                el.child(self.render_section("Translation", &translation, keyed, disabled, cx))
                    .child(self.render_section("Rotation", &rotation, keyed, disabled, cx))
                    .child(self.render_section("Scale", &scale, keyed, disabled, cx))
            })
            .when(!has_selection, |el| {
                el.child(
                    div()
                        .p_4()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Select a bone to edit its transform."),
                )
            })
    }
}
