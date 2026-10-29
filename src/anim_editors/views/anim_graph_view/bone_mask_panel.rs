//! The bone masks of a Layered Blend per Bone node in the inspector: for each
//! layer a default weight, ordered branches (bone, depth, weight), presets and
//! the weight every bone ends up with. Every control is one undoable property
//! edit of the node's `masks`, checked against the graph's skeleton.

use gpui::*;
use graphy_blueprint::NodeInstance;
use skeletal_assets::skeleton::Skeleton;
use ui::button::{Button, ButtonVariants};
use ui::{h_flex, v_flex, ActiveTheme, IconName, Sizable, StyledExt};

use super::*;
use crate::anim_editors::bone_mask_edit::{self as masks, BranchField};

fn skeleton_of(editor: &AnimGraphEditor) -> Option<Skeleton> {
    let id = editor.model.asset().skeleton.as_ref()?.as_str().to_string();
    skeleton_canvas::load_skeleton(&editor.chrome.root, &id)
}

/// The rows for the `masks` property of `node`.
pub(super) fn rows(editor: &mut AnimGraphEditor, node: &NodeInstance, label: &str, window: &mut Window, cx: &mut Context<AnimGraphEditor>) -> Vec<AnyElement> {
    let theme = cx.theme().clone();
    let layers = node.properties.get("count").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
    let specs = masks::read(node.properties.get("masks").unwrap_or(&Value::Null), layers);
    let skeleton = skeleton_of(editor);
    let mut out: Vec<AnyElement> = vec![div().text_xs().font_semibold().text_color(theme.muted_foreground).child(label.to_string()).into_any_element()];
    if skeleton.is_none() {
        out.push(div().text_xs().text_color(theme.warning).child("Pick the graph's skeleton to name bones and see the weights.").into_any_element());
    }
    for (layer, spec) in specs.iter().enumerate() {
        let id = node.id.clone();
        let summary = match &skeleton {
            Some(sk) => {
                let (covered, total) = masks::coverage(spec, sk);
                format!("{covered} of {total} bones take layer {layer}")
            }
            None => format!("{} branches", spec.branches.len()),
        };
        let mut card = v_flex().w_full().gap_1().p_2().rounded(px(6.0)).border_1().border_color(theme.border);
        card = card.child(div().text_xs().font_semibold().text_color(theme.foreground).child(format!("Layer {layer}")));
        card = card.child(div().text_xs().text_color(theme.muted_foreground).child(summary));
        card = card.child(editor.widgets.text_field(&format!("mask.{layer}.default"), "Other bones", &fmt_f32(spec.default_weight), window, cx));

        for (i, branch) in spec.branches.iter().enumerate() {
            let shown_bone = skeleton.as_ref().and_then(|sk| sk.bone(&branch.bone)).map(|b| b.name.clone()).unwrap_or_else(|| branch.bone.clone());
            let depth = branch.depth.map(|d| d.to_string()).unwrap_or_default();
            let (up, down, remove) = (id.clone(), id.clone(), id.clone());
            card = card.child(
                v_flex()
                    .gap_1()
                    .pt_1()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(editor.widgets.text_field(&format!("mask.{layer}.{i}.bone"), &format!("Branch {} bone", i + 1), &shown_bone, window, cx))
                    .child(editor.widgets.text_field(&format!("mask.{layer}.{i}.weight"), "Weight", &fmt_f32(branch.weight), window, cx))
                    .child(editor.widgets.text_field(&format!("mask.{layer}.{i}.depth"), "Depth (blank = all below)", &depth, window, cx))
                    .child(
                        h_flex()
                            .gap_1()
                            .child(Button::new(SharedString::from(format!("mask-up-{layer}-{i}"))).icon(IconName::ArrowUp).ghost().xsmall().tooltip("Earlier: later branches win overlaps").on_click(cx.listener(move |this, _, window, cx| {
                                let sk = skeleton_of(this);
                                let n = up.clone();
                                edit(this, window, cx, |m| m.edit_mask(&n, layer, sk.as_ref(), |s| if masks::move_branch(s, i, false) { Ok(()) } else { Err("already first".into()) }));
                                this.push_to_canvas(false, cx);
                            })))
                            .child(Button::new(SharedString::from(format!("mask-down-{layer}-{i}"))).icon(IconName::ArrowDown).ghost().xsmall().tooltip("Later: wins overlaps").on_click(cx.listener(move |this, _, window, cx| {
                                let sk = skeleton_of(this);
                                let n = down.clone();
                                edit(this, window, cx, |m| m.edit_mask(&n, layer, sk.as_ref(), |s| if masks::move_branch(s, i, true) { Ok(()) } else { Err("already last".into()) }));
                                this.push_to_canvas(false, cx);
                            })))
                            .child(Button::new(SharedString::from(format!("mask-remove-{layer}-{i}"))).icon(IconName::Trash).label("Remove").ghost().xsmall().on_click(cx.listener(move |this, _, window, cx| {
                                let sk = skeleton_of(this);
                                let n = remove.clone();
                                edit(this, window, cx, |m| m.edit_mask(&n, layer, sk.as_ref(), |s| if masks::remove_branch(s, i) { Ok(()) } else { Err("no such branch".into()) }));
                                this.push_to_canvas(false, cx);
                            }))),
                    ),
            );
        }

        let first_bone = skeleton.as_ref().and_then(|sk| sk.bones.first()).map(|b| b.id.clone());
        let (add, all, none, invert) = (id.clone(), id.clone(), id.clone(), id.clone());
        card = card.child(
            h_flex()
                .gap_1()
                .flex_wrap()
                .pt_1()
                .child(Button::new(SharedString::from(format!("mask-add-{layer}"))).icon(IconName::Plus).label("Add branch").outline().xsmall().on_click(cx.listener(move |this, _, window, cx| {
                    let sk = skeleton_of(this);
                    let n = add.clone();
                    let bone = first_bone.clone();
                    edit(this, window, cx, |m| {
                        let bone = bone.ok_or("Pick the graph's skeleton first")?;
                        m.edit_mask(&n, layer, sk.as_ref(), |s| {
                            masks::add_branch(s, &bone);
                            Ok(())
                        })
                    });
                    this.push_to_canvas(false, cx);
                })))
                .child(Button::new(SharedString::from(format!("mask-all-{layer}"))).label("Everything").ghost().xsmall().on_click(cx.listener(move |this, _, window, cx| {
                    let n = all.clone();
                    edit(this, window, cx, |m| m.edit_mask(&n, layer, None, |s| Ok(masks::apply_preset(s, &masks::Preset::Everything))));
                    this.push_to_canvas(false, cx);
                })))
                .child(Button::new(SharedString::from(format!("mask-none-{layer}"))).label("Nothing").ghost().xsmall().on_click(cx.listener(move |this, _, window, cx| {
                    let n = none.clone();
                    edit(this, window, cx, |m| m.edit_mask(&n, layer, None, |s| Ok(masks::apply_preset(s, &masks::Preset::Nothing))));
                    this.push_to_canvas(false, cx);
                })))
                .child(Button::new(SharedString::from(format!("mask-invert-{layer}"))).label("Invert").ghost().xsmall().tooltip("Swap what the mask takes and leaves").on_click(cx.listener(move |this, _, window, cx| {
                    let n = invert.clone();
                    edit(this, window, cx, |m| m.edit_mask(&n, layer, None, |s| Ok(masks::invert(s))));
                    this.push_to_canvas(false, cx);
                }))),
        );

        if let Some(sk) = &skeleton {
            let weights = masks::effective(spec, sk);
            let list: Vec<AnyElement> = weights
                .into_iter()
                .map(|(name, w)| {
                    h_flex()
                        .justify_between()
                        .child(div().text_xs().text_color(if w > 0.0 { theme.foreground } else { theme.muted_foreground }).child(name))
                        .child(div().text_xs().text_color(theme.muted_foreground).child(fmt_f32(w)))
                        .into_any_element()
                })
                .collect();
            card = card.child(section("Weight of each bone", list, cx));
        }
        out.push(card.into_any_element());
    }
    out
}

/// Commit a mask field. `rest` is the key after `mask.`: `<layer>.default` or
/// `<layer>.<branch>.<bone|weight|depth>`.
pub(super) fn commit(editor: &mut AnimGraphEditor, rest: &str, text: String, window: &mut Window, cx: &mut Context<AnimGraphEditor>) {
    let AnimSelection::GraphNode { id } = editor.model.selection.clone() else { return };
    let parts: Vec<String> = rest.split('.').map(str::to_string).collect();
    let Some(layer) = parts.first().and_then(|l| l.parse::<usize>().ok()) else { return };
    let skeleton = skeleton_of(editor);
    let number = parse_f32(&text);
    edit(editor, window, cx, |m| {
        m.edit_mask(&id, layer, skeleton.as_ref(), |spec| match parts.as_slice() {
            [_, d] if d == "default" => masks::set_default_weight(spec, number.ok_or("enter a number between 0 and 1")?),
            [_, i, field] => {
                let i: usize = i.parse().map_err(|_| "no such branch".to_string())?;
                let field = match field.as_str() {
                    "weight" => BranchField::Weight(number.ok_or("enter a number between 0 and 1")?),
                    "depth" => BranchField::Depth(if text.trim().is_empty() {
                        None
                    } else {
                        Some(text.trim().parse::<u32>().map_err(|_| "depth is a whole number, or blank for everything below".to_string())?)
                    }),
                    "bone" => {
                        let sk = skeleton.as_ref().ok_or("Pick the graph's skeleton first")?;
                        BranchField::Bone(masks::resolve_bone(sk, &text).ok_or_else(|| format!("the skeleton has no bone `{}`", text.trim()))?)
                    }
                    _ => return Ok(()),
                };
                masks::set_branch(spec, i, field)
            }
            _ => Ok(()),
        })
    });
    editor.push_to_canvas(false, cx);
}
