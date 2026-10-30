//! The bone masks of a Layered Blend per Bone node in the inspector, and the
//! skeleton's named mask library.
//!
//! For each layer: a mask authored inline (default weight, ordered branches,
//! presets, the weight every bone ends up with) or a reference to a named mask
//! stored on the skeleton. Every layer control is one undoable property edit
//! of the node's `masks`, checked against the graph's skeleton. The library
//! section edits the skeleton asset itself and saves it immediately.

use gpui::*;
use graphy_blueprint::NodeInstance;
use skeletal_assets::bone_mask::{BoneMaskSpec, NamedBoneMask};
use skeletal_assets::skeleton::Skeleton;
use ui::button::{Button, ButtonVariants};
use ui::{h_flex, v_flex, ActiveTheme, IconName, Sizable, StyledExt};

use super::*;
use crate::anim_editors::bone_mask_edit::{self as masks, BranchField};
use crate::anim_editors::skeleton_masks::{self, MaskOp};

fn skeleton_id(editor: &AnimGraphEditor) -> Option<String> {
    editor.model.asset().skeleton.as_ref().map(|s| s.as_str().to_string())
}

fn skeleton_of(editor: &AnimGraphEditor) -> Option<Skeleton> {
    skeleton_canvas::load_skeleton(&editor.chrome.root, &skeleton_id(editor)?)
}

fn library_of(editor: &AnimGraphEditor) -> Vec<NamedBoneMask> {
    skeleton_id(editor).map(|id| skeleton_masks::library(&editor.chrome.root, &id)).unwrap_or_default()
}

/// Run `change` on the skeleton file, report a failure in the status line, and refresh the preview.
fn update_library(editor: &mut AnimGraphEditor, window: &mut Window, cx: &mut Context<AnimGraphEditor>, change: impl FnOnce(&mut skeletal_assets::assets::SkeletonAsset) -> Result<(), String>) {
    let result = match skeleton_id(editor) {
        Some(id) => skeleton_masks::update(&editor.chrome.root, &id, change).map(|_| ()),
        None => Err("Pick the graph's skeleton first".into()),
    };
    editor.chrome.status = result.err();
    editor.after_change(window, cx);
    editor.push_to_canvas(false, cx);
    cx.notify();
}

/// The rows for the `masks` property of `node`.
pub(super) fn rows(editor: &mut AnimGraphEditor, node: &NodeInstance, label: &str, window: &mut Window, cx: &mut Context<AnimGraphEditor>) -> Vec<AnyElement> {
    let theme = cx.theme().clone();
    let layers = node.properties.get("count").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
    let specs = masks::read(node.properties.get("masks").unwrap_or(&Value::Null), layers);
    let skeleton = skeleton_of(editor);
    let library = library_of(editor);
    let mut out: Vec<AnyElement> = vec![div().text_xs().font_semibold().text_color(theme.muted_foreground).child(label.to_string()).into_any_element()];
    if skeleton.is_none() {
        out.push(div().text_xs().text_color(theme.warning).child("Pick the graph's skeleton to name bones and see the weights.").into_any_element());
    }
    for (layer, spec) in specs.iter().enumerate() {
        let id = node.id.clone();
        let summary = match &skeleton {
            Some(sk) => {
                let (covered, total) = masks::coverage(spec, sk, &library);
                format!("{covered} of {total} bones take layer {layer}")
            }
            None => format!("{} branches", spec.branches.len()),
        };
        let mut card = v_flex().w_full().gap_1().p_2().rounded(px(6.0)).border_1().border_color(theme.border);
        card = card.child(div().text_xs().font_semibold().text_color(theme.foreground).child(format!("Layer {layer}")));
        card = card.child(div().text_xs().text_color(theme.muted_foreground).child(summary));
        card = card.child(source_row(&id, layer, spec, &library, cx));

        if let Some(name) = &spec.profile {
            let known = library.iter().any(|m| &m.name == name);
            card = card.child(div().text_xs().text_color(if known { theme.muted_foreground } else { theme.danger }).child(if known {
                format!("Follows the skeleton mask `{name}`. Edit the mask under Skeleton Masks to change every user.")
            } else {
                format!("The skeleton has no mask named `{name}`. Pick another or detach to an inline copy.")
            }));
        } else {
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
                                    let (sk, lib) = (skeleton_of(this), library_of(this));
                                    let n = up.clone();
                                    edit(this, window, cx, |m| m.edit_mask(&n, layer, sk.as_ref(), &lib, |s| if masks::move_branch(s, i, false) { Ok(()) } else { Err("already first".into()) }));
                                    this.push_to_canvas(false, cx);
                                })))
                                .child(Button::new(SharedString::from(format!("mask-down-{layer}-{i}"))).icon(IconName::ArrowDown).ghost().xsmall().tooltip("Later: wins overlaps").on_click(cx.listener(move |this, _, window, cx| {
                                    let (sk, lib) = (skeleton_of(this), library_of(this));
                                    let n = down.clone();
                                    edit(this, window, cx, |m| m.edit_mask(&n, layer, sk.as_ref(), &lib, |s| if masks::move_branch(s, i, true) { Ok(()) } else { Err("already last".into()) }));
                                    this.push_to_canvas(false, cx);
                                })))
                                .child(Button::new(SharedString::from(format!("mask-remove-{layer}-{i}"))).icon(IconName::Trash).label("Remove").ghost().xsmall().on_click(cx.listener(move |this, _, window, cx| {
                                    let (sk, lib) = (skeleton_of(this), library_of(this));
                                    let n = remove.clone();
                                    edit(this, window, cx, |m| m.edit_mask(&n, layer, sk.as_ref(), &lib, |s| if masks::remove_branch(s, i) { Ok(()) } else { Err("no such branch".into()) }));
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
                        let (sk, lib) = (skeleton_of(this), library_of(this));
                        let n = add.clone();
                        let bone = first_bone.clone();
                        edit(this, window, cx, |m| {
                            let bone = bone.ok_or("Pick the graph's skeleton first")?;
                            m.edit_mask(&n, layer, sk.as_ref(), &lib, |s| {
                                masks::add_branch(s, &bone);
                                Ok(())
                            })
                        });
                        this.push_to_canvas(false, cx);
                    })))
                    .child(Button::new(SharedString::from(format!("mask-all-{layer}"))).label("Everything").ghost().xsmall().on_click(cx.listener(move |this, _, window, cx| {
                        let n = all.clone();
                        edit(this, window, cx, |m| m.edit_mask(&n, layer, None, &[], |s| Ok(masks::apply_preset(s, &masks::Preset::Everything))));
                        this.push_to_canvas(false, cx);
                    })))
                    .child(Button::new(SharedString::from(format!("mask-none-{layer}"))).label("Nothing").ghost().xsmall().on_click(cx.listener(move |this, _, window, cx| {
                        let n = none.clone();
                        edit(this, window, cx, |m| m.edit_mask(&n, layer, None, &[], |s| Ok(masks::apply_preset(s, &masks::Preset::Nothing))));
                        this.push_to_canvas(false, cx);
                    })))
                    .child(Button::new(SharedString::from(format!("mask-invert-{layer}"))).label("Invert").ghost().xsmall().tooltip("Swap what the mask takes and leaves").on_click(cx.listener(move |this, _, window, cx| {
                        let n = invert.clone();
                        edit(this, window, cx, |m| m.edit_mask(&n, layer, None, &[], |s| Ok(masks::invert(s))));
                        this.push_to_canvas(false, cx);
                    }))),
            );
        }

        if let Some(sk) = &skeleton {
            let weights = masks::effective(spec, sk, &library);
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

/// The row that chooses where a layer's mask comes from: one button per skeleton mask, plus
/// "Save as skeleton mask" for an inline mask and "Detach" for a named one.
fn source_row(node: &str, layer: usize, spec: &BoneMaskSpec, library: &[NamedBoneMask], cx: &mut Context<AnimGraphEditor>) -> AnyElement {
    let mut row = h_flex().gap_1().flex_wrap().items_center().child(div().text_xs().text_color(cx.theme().muted_foreground).child("Mask from:"));
    for mask in library {
        let (n, name) = (node.to_string(), mask.name.clone());
        let on = spec.profile.as_deref() == Some(mask.name.as_str());
        let mut button = Button::new(SharedString::from(format!("mask-use-{layer}-{}", mask.name))).label(mask.name.clone()).xsmall();
        button = if on { button.primary() } else { button.outline() };
        row = row.child(button.on_click(cx.listener(move |this, _, window, cx| {
            let (sk, lib) = (skeleton_of(this), library_of(this));
            let name = name.clone();
            edit(this, window, cx, |m| m.edit_mask(&n, layer, sk.as_ref(), &lib, |s| Ok(masks::use_named(s, &name))));
            this.push_to_canvas(false, cx);
        })));
    }
    if spec.profile.is_some() {
        let n = node.to_string();
        row = row.child(Button::new(SharedString::from(format!("mask-detach-{layer}"))).label("Detach").ghost().xsmall().tooltip("Copy the skeleton mask into this layer and author it here").on_click(cx.listener(move |this, _, window, cx| {
            let (sk, lib) = (skeleton_of(this), library_of(this));
            let lib2 = lib.clone();
            edit(this, window, cx, |m| m.edit_mask(&n, layer, sk.as_ref(), &lib, |s| masks::detach(s, &lib2)));
            this.push_to_canvas(false, cx);
        })));
    } else if !spec.branches.is_empty() {
        let n = node.to_string();
        row = row.child(Button::new(SharedString::from(format!("mask-save-{layer}"))).label("Save as skeleton mask").ghost().xsmall().tooltip("Store this mask on the skeleton so other layers and Montages can use it").on_click(cx.listener(move |this, _, window, cx| {
            promote_layer(this, &n, layer, window, cx);
        })));
    }
    row.into_any_element()
}

/// Store an inline layer mask on the skeleton and make the layer follow it.
fn promote_layer(editor: &mut AnimGraphEditor, node: &str, layer: usize, window: &mut Window, cx: &mut Context<AnimGraphEditor>) {
    let Some(n) = editor.model.graph().nodes.get(node).cloned() else { return };
    let layers = n.properties.get("count").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
    let mut spec = masks::read(n.properties.get("masks").unwrap_or(&Value::Null), layers).remove(layer.min(layers - 1));
    let hint = format!("{} layer {}", editor.model.asset().name, layer + 1);
    let mut promoted = None;
    update_library(editor, window, cx, |asset| {
        promoted = skeletal_assets::mask_library::promote(&mut spec, &mut asset.masks, &hint);
        skeletal_assets::bone_mask::validate_library(&asset.masks, &asset.skeleton())
    });
    if let Some(name) = promoted {
        let (sk, lib) = (skeleton_of(editor), library_of(editor));
        let node = node.to_string();
        edit(editor, window, cx, |m| m.edit_mask(&node, layer, sk.as_ref(), &lib, |s| Ok(masks::use_named(s, &name))));
        editor.push_to_canvas(false, cx);
    }
}

/// Commit a mask field. `rest` is the key after `mask.`: `<layer>.default` or
/// `<layer>.<branch>.<bone|weight|depth>`.
pub(super) fn commit(editor: &mut AnimGraphEditor, rest: &str, text: String, window: &mut Window, cx: &mut Context<AnimGraphEditor>) {
    let AnimSelection::GraphNode { id } = editor.model.selection.clone() else { return };
    let parts: Vec<String> = rest.split('.').map(str::to_string).collect();
    let Some(layer) = parts.first().and_then(|l| l.parse::<usize>().ok()) else { return };
    let (skeleton, lib) = (skeleton_of(editor), library_of(editor));
    let number = parse_f32(&text);
    edit(editor, window, cx, |m| {
        m.edit_mask(&id, layer, skeleton.as_ref(), &lib, |spec| match parts.as_slice() {
            [_, d] if d == "default" => masks::set_default_weight(spec, number.ok_or("enter a number between 0 and 1")?),
            [_, i, field] => {
                let i: usize = i.parse().map_err(|_| "no such branch".to_string())?;
                let field = field_of(field, &text, number, skeleton.as_ref())?;
                masks::set_branch(spec, i, field)
            }
            _ => Ok(()),
        })
    });
    editor.push_to_canvas(false, cx);
}

fn field_of(field: &str, text: &str, number: Option<f32>, skeleton: Option<&Skeleton>) -> Result<BranchField, String> {
    Ok(match field {
        "weight" => BranchField::Weight(number.ok_or("enter a number between 0 and 1")?),
        "depth" => BranchField::Depth(if text.trim().is_empty() {
            None
        } else {
            Some(text.trim().parse::<u32>().map_err(|_| "depth is a whole number, or blank for everything below".to_string())?)
        }),
        "bone" => {
            let sk = skeleton.ok_or("Pick the graph's skeleton first")?;
            BranchField::Bone(masks::resolve_bone(sk, text).ok_or_else(|| format!("the skeleton has no bone `{}`", text.trim()))?)
        }
        _ => return Err("unknown field".into()),
    })
}

// ---- the skeleton's mask library ---------------------------------------------------------

/// The Skeleton Masks section of the graph inspector: create, rename, edit and delete the
/// named masks of the graph's skeleton.
pub(super) fn library_rows(editor: &mut AnimGraphEditor, window: &mut Window, cx: &mut Context<AnimGraphEditor>) -> AnyElement {
    let theme = cx.theme().clone();
    let skeleton = skeleton_of(editor);
    let library = library_of(editor);
    let mut body = v_flex().w_full().gap_2();
    let Some(sk) = skeleton else {
        body = body.child(div().text_xs().text_color(theme.warning).child("Pick the graph's skeleton to author its masks."));
        return section("Skeleton Masks", vec![body.into_any_element()], cx);
    };
    for (index, mask) in library.iter().enumerate() {
        let (covered, total) = masks::coverage(&mask.spec, &sk, &[]);
        let name = mask.name.clone();
        let (del, all, none, invert) = (name.clone(), name.clone(), name.clone(), name.clone());
        let mut card = v_flex().w_full().gap_1().p_2().rounded(px(6.0)).border_1().border_color(theme.border);
        card = card.child(editor.widgets.text_field(&format!("skmask.{index}.name"), "Name", &mask.name, window, cx));
        card = card.child(div().text_xs().text_color(theme.muted_foreground).child(format!("{covered} of {total} bones")));
        card = card.child(editor.widgets.text_field(&format!("skmask.{index}.default"), "Other bones", &fmt_f32(mask.spec.default_weight), window, cx));
        for (i, branch) in mask.spec.branches.iter().enumerate() {
            let shown = sk.bone(&branch.bone).map(|b| b.name.clone()).unwrap_or_else(|| branch.bone.clone());
            let depth = branch.depth.map(|d| d.to_string()).unwrap_or_default();
            let remove = name.clone();
            card = card.child(
                v_flex()
                    .gap_1()
                    .pt_1()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(editor.widgets.text_field(&format!("skmask.{index}.{i}.bone"), &format!("Branch {} bone", i + 1), &shown, window, cx))
                    .child(editor.widgets.text_field(&format!("skmask.{index}.{i}.weight"), "Weight", &fmt_f32(branch.weight), window, cx))
                    .child(editor.widgets.text_field(&format!("skmask.{index}.{i}.depth"), "Depth (blank = all below)", &depth, window, cx))
                    .child(Button::new(SharedString::from(format!("skmask-remove-{index}-{i}"))).icon(IconName::Trash).label("Remove branch").ghost().xsmall().on_click(cx.listener(move |this, _, window, cx| {
                        let name = remove.clone();
                        update_library(this, window, cx, |a| skeleton_masks::edit(a, &name, |s| if masks::remove_branch(s, i) { Ok(()) } else { Err("no such branch".into()) }));
                    }))),
            );
        }
        let (add, first) = (name.clone(), sk.bones.first().map(|b| b.id.clone()));
        card = card.child(
            h_flex()
                .gap_1()
                .flex_wrap()
                .pt_1()
                .child(Button::new(SharedString::from(format!("skmask-add-{index}"))).icon(IconName::Plus).label("Add branch").outline().xsmall().on_click(cx.listener(move |this, _, window, cx| {
                    let (name, bone) = (add.clone(), first.clone());
                    update_library(this, window, cx, |a| {
                        let bone = bone.ok_or("The skeleton has no bones")?;
                        skeleton_masks::edit(a, &name, |s| {
                            masks::add_branch(s, &bone);
                            Ok(())
                        })
                    });
                })))
                .child(Button::new(SharedString::from(format!("skmask-all-{index}"))).label("Everything").ghost().xsmall().on_click(cx.listener(move |this, _, window, cx| {
                    let name = all.clone();
                    update_library(this, window, cx, |a| skeleton_masks::edit(a, &name, |s| Ok(masks::apply_preset(s, &masks::Preset::Everything))));
                })))
                .child(Button::new(SharedString::from(format!("skmask-none-{index}"))).label("Nothing").ghost().xsmall().on_click(cx.listener(move |this, _, window, cx| {
                    let name = none.clone();
                    update_library(this, window, cx, |a| skeleton_masks::edit(a, &name, |s| Ok(masks::apply_preset(s, &masks::Preset::Nothing))));
                })))
                .child(Button::new(SharedString::from(format!("skmask-invert-{index}"))).label("Invert").ghost().xsmall().on_click(cx.listener(move |this, _, window, cx| {
                    let name = invert.clone();
                    update_library(this, window, cx, |a| skeleton_masks::edit(a, &name, |s| Ok(masks::invert(s))));
                })))
                .child(Button::new(SharedString::from(format!("skmask-delete-{index}"))).icon(IconName::Trash).label("Delete mask").danger().xsmall().on_click(cx.listener(move |this, _, window, cx| {
                    let name = del.clone();
                    update_library(this, window, cx, |a| skeleton_masks::apply(a, MaskOp::Delete { name }).map(|_| ()));
                }))),
        );
        body = body.child(card);
    }
    let first_bone = sk.bones.first().map(|b| b.id.clone());
    body = body.child(h_flex().gap_1().child(Button::new("skmask-new").icon(IconName::Plus).label("New mask").outline().xsmall().on_click(cx.listener(move |this, _, window, cx| {
        let bone = first_bone.clone();
        update_library(this, window, cx, |a| {
            let bone = bone.ok_or("The skeleton has no bones")?;
            let mut spec = BoneMaskSpec::default();
            masks::apply_preset(&mut spec, &masks::Preset::Subtree(bone));
            skeleton_masks::apply(a, MaskOp::Add { name: "Mask".into(), spec }).map(|_| ())
        });
    }))));
    body = body.child(div().text_xs().text_color(theme.muted_foreground).child("Masks are stored on the skeleton and saved at once. Layers and Montage slots that name a mask follow its edits; renaming or deleting a mask other assets use makes them report a missing mask."));
    section("Skeleton Masks", vec![body.into_any_element()], cx)
}

/// Commit a library field. `rest` is the key after `skmask.`: `<index>.name`, `<index>.default`
/// or `<index>.<branch>.<bone|weight|depth>`.
pub(super) fn commit_library(editor: &mut AnimGraphEditor, rest: &str, text: String, window: &mut Window, cx: &mut Context<AnimGraphEditor>) {
    let parts: Vec<String> = rest.split('.').map(str::to_string).collect();
    let Some(index) = parts.first().and_then(|l| l.parse::<usize>().ok()) else { return };
    let library = library_of(editor);
    let Some(name) = library.get(index).map(|m| m.name.clone()) else { return };
    let skeleton = skeleton_of(editor);
    let number = parse_f32(&text);
    update_library(editor, window, cx, |asset| match parts.as_slice() {
        [_, f] if f == "name" => skeleton_masks::apply(asset, MaskOp::Rename { from: name.clone(), to: text.clone() }).map(|_| ()),
        [_, f] if f == "default" => skeleton_masks::edit(asset, &name, |s| masks::set_default_weight(s, number.ok_or("enter a number between 0 and 1")?)),
        [_, i, field] => {
            let i: usize = i.parse().map_err(|_| "no such branch".to_string())?;
            let field = field_of(field, &text, number, skeleton.as_ref())?;
            skeleton_masks::edit(asset, &name, |s| masks::set_branch(s, i, field))
        }
        _ => Ok(()),
    });
}
