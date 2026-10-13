//! Small building blocks shared by every panel, so the editor's chrome matches
//! the rest of the engine (the same "Details"-style header the Blueprint and
//! Shader editors use).

use gpui::*;
use ui::{h_flex, ActiveTheme, Icon, IconName, StyledExt};

/// Monospace face used for numbers (timecode, ruler, axis values).
pub const MONO: &str = "JetBrainsMono-Regular";

/// Panel title bar: icon, title, and optional trailing content on the right.
pub fn panel_header(
    icon: IconName,
    title: impl Into<SharedString>,
    trailing: Option<AnyElement>,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    h_flex()
        .w_full()
        .flex_shrink_0()
        .h(px(32.0))
        .px_2()
        .gap_2()
        .items_center()
        .bg(theme.secondary)
        .border_b_1()
        .border_color(theme.border)
        .child(Icon::new(icon).size(px(16.0)).text_color(theme.info))
        .child(
            div()
                .text_sm()
                .font_semibold()
                .text_color(theme.foreground)
                .child(title.into()),
        )
        .child(div().flex_1())
        .children(trailing)
}

/// Thin vertical rule between toolbar groups.
pub fn toolbar_divider(cx: &App) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .w(px(1.0))
        .h(px(18.0))
        .mx_1()
        .bg(cx.theme().border)
}

/// Small caption used above groups of controls.
pub fn caption(text: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text.into())
}

/// Centered icon + message shown when a panel has nothing to display.
pub fn empty_message(
    icon: IconName,
    title: impl Into<SharedString>,
    hint: impl Into<SharedString>,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .p_4()
        .child(
            Icon::new(icon)
                .size(px(28.0))
                .text_color(theme.muted_foreground.opacity(0.55)),
        )
        .child(
            div()
                .text_sm()
                .font_medium()
                .text_color(theme.foreground.opacity(0.85))
                .child(title.into()),
        )
        .child(
            div()
                .text_xs()
                .text_center()
                .text_color(theme.muted_foreground)
                .child(hint.into()),
        )
}
