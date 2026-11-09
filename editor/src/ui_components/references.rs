//! The "Used By" card of the Skeleton and Skeletal Mesh editors: what in the
//! project references this asset, read from the derived dependency index
//! (never stored in the asset), with a click to open the referrer.

use std::path::Path;

use gpui::prelude::FluentBuilder;
use gpui::*;
use plugin_editor_api::OpenAsset;
use ui::{h_flex, v_flex, ActiveTheme, StyledExt};

use crate::core::asset_index;

fn kind_label(kind: &str) -> &str {
    match kind {
        "animseq" => "Sequence",
        "skmesh" => "Skeletal Mesh",
        "blendspace" => "Blend Space",
        "anim_montage" => "Montage",
        "anim_graph" => "Animation Graph",
        "anim_retarget" => "Retarget Profile",
        "ik_rig" => "IK Rig",
        "ik_retarget" => "IK Retargeter",
        "pose_db" => "Pose Database",
        other => other,
    }
}

/// Referrers of `(kind, id)` as rows: file name, kind, and the field that
/// holds the reference. `title` heads the card.
pub fn used_by_card(root: &Path, kind: &str, id: &str, title: &str, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let referrers = asset_index::used_by(root, kind, id);
    let mut card = v_flex().w_full().px_3().py_2().gap_1().border_t_1().border_color(theme.border).child(
        div().text_xs().font_semibold().text_color(theme.muted_foreground).child(format!("{} ({})", title.to_uppercase(), referrers.len())),
    );
    if referrers.is_empty() {
        card = card.child(div().text_xs().text_color(theme.muted_foreground).child("Nothing in the project references this asset."));
    }
    for (i, r) in referrers.into_iter().take(60).enumerate() {
        let name = r.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let path = r.path.clone();
        card = card.child(
            h_flex()
                .id(SharedString::from(format!("used-by-{i}")))
                .w_full()
                .gap_2()
                .py_0p5()
                .items_center()
                .cursor_pointer()
                .hover(|s| s.bg(theme.secondary))
                .text_xs()
                .child(div().text_color(theme.foreground).child(name))
                .child(div().text_color(theme.muted_foreground).child(kind_label(&r.kind).to_string()))
                .child(div().flex_1().text_color(theme.muted_foreground).child(r.via.clone()))
                .when(r.hint, |row| row.child(div().text_color(theme.muted_foreground).child("hint")))
                .on_click(move |_, window, cx| window.dispatch_action(Box::new(OpenAsset { path: path.clone() }), cx)),
        );
    }
    card.into_any_element()
}
