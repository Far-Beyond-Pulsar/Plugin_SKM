//! The IK Rig editor: the skeleton with chain, goal and joint-limit overlays,
//! a hierarchy tree with badges, the ordered solver stack, and an inspector for
//! chains, goals, solvers and limits. Picking a bone in the viewport or the
//! tree selects what it is (goal, limit or plain bone).

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::prelude::FluentBuilder;
use gpui::*;
use plugin_editor_api::PluginError;
use pulsar_reflection::{AssetKind, AssetRef, SkeletonAssetKind};
use skeletal_assets::ik_rig::{IkRigAsset, PoleSettings, SolverKind};
use ui::button::{Button, ButtonVariants};
use ui::{h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable, StyledExt};

use crate::anim_editors::ik_rig_model::IkRigModel;
use crate::anim_editors::refs::{self, RefStatus, SnapshotIndex};
use crate::anim_editors::selection::AnimSelection;
use crate::anim_editors::skeleton_proj::SkeletonProjection;
use crate::anim_editors::store::FsStore;
use crate::core::asset_index;

use super::chrome::{edit, empty_inspector, fmt_f32, frame, labelled, parse_f32, value_row, Chrome, EditorBody, EditorHost, EditorModel, Widgets};
use super::skeleton_canvas::{self, load_skeleton, Overlay, Ring};

const PALETTE: [(f32, f32, f32); 6] = [(0.55, 0.8, 0.6), (0.75, 0.6, 0.7), (0.9, 0.7, 0.65), (0.4, 0.7, 0.55), (0.12, 0.85, 0.6), (0.97, 0.75, 0.62)];

fn chain_color(i: usize) -> Hsla {
    let (h, s, l) = PALETTE[i % PALETTE.len()];
    hsla(h, s, l, 0.95)
}

pub struct IkRigEditor {
    model: IkRigModel,
    chrome: Chrome,
    widgets: Widgets,
    projection: SkeletonProjection,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    statuses: std::collections::HashMap<String, RefStatus>,
    /// The first bone picked when making a chain.
    chain_start: Option<String>,
}

impl IkRigEditor {
    pub fn open(path: PathBuf, _window: &mut Window, cx: &mut App) -> Result<Entity<Self>, PluginError> {
        let text = IkRigAsset::new("New IK Rig", None).to_text().unwrap_or_default();
        let model = IkRigModel::open(Arc::new(FsStore), &path, &text).map_err(|message| PluginError::Other { message })?;
        let chrome = Chrome::new(&path, cx);
        asset_index::record_asset(&chrome.root, &path);
        let mut this = Self { model, chrome, widgets: Widgets::default(), projection: SkeletonProjection::default(), bounds: Rc::new(Cell::new(Bounds::default())), statuses: Default::default(), chain_start: None };
        this.reload_skeleton();
        Ok(cx.new(|_| this))
    }

    fn reload_skeleton(&mut self) {
        let id = self.model.asset().skeleton.as_ref().map(|s| s.as_str().to_string());
        self.model.skeleton = id.as_deref().and_then(|id| load_skeleton(&self.chrome.root, id));
        self.projection = self.model.skeleton.as_ref().map(SkeletonProjection::new).unwrap_or_default();
        let index = SnapshotIndex::new(&self.chrome.root);
        self.statuses.insert("skeleton".into(), refs::ref_status(&index, SkeletonAssetKind::KIND, id.as_deref(), None));
    }

    fn bone_name(&self, id: &str) -> String {
        self.model.skeleton.as_ref().and_then(|s| s.bone(id)).map_or_else(|| id.to_string(), |b| b.name.clone())
    }

    fn selected_bone(&self) -> Option<String> {
        match &self.model.selection {
            AnimSelection::Bone { id } => Some(id.clone()),
            AnimSelection::JointLimit { bone } => Some(bone.clone()),
            AnimSelection::IkGoal { name } => self.model.asset().goals.iter().find(|g| &g.name == name).map(|g| g.bone.clone()),
            _ => None,
        }
    }

    fn viewport(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let overlays: Vec<Overlay> = self
            .model
            .chain_overlays()
            .iter()
            .enumerate()
            .map(|(i, o)| Overlay { bones: o.bones.clone(), color: if o.valid { chain_color(i) } else { theme.danger }, width: if matches!(&self.model.selection, AnimSelection::IkChain { name } if *name == o.name) { 5.0 } else { 3.0 } })
            .collect();
        let rings: Vec<Ring> = self.model.asset().goals.iter().map(|g| Ring { bone: g.bone.clone(), color: theme.success }).collect();
        let selected = self.selected_bone();
        let element = skeleton_canvas::element(self.projection.clone(), self.bounds.clone(), theme.foreground, theme.background, overlays, rings, selected, theme.primary);
        let projection = self.projection.clone();
        let bounds = self.bounds.clone();
        div()
            .id("ik-viewport")
            .size_full()
            .bg(theme.background)
            .child(element)
            .child(if self.projection.is_empty() {
                div().absolute().top(px(12.0)).left(px(12.0)).text_xs().text_color(theme.muted_foreground).child("Pick the rig's Skeleton in the inspector to see its bones.").into_any_element()
            } else {
                div().into_any_element()
            })
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                if let Some(bone) = skeleton_canvas::pick(&projection, &bounds, e.position, 12.0) {
                    this.model.pick_bone(&bone);
                } else {
                    this.model.clear_selection();
                }
                cx.notify();
            }))
            .into_any_element()
    }

    fn solver_stack(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let rows = self.model.solver_rows();
        let selected = match self.model.selection {
            AnimSelection::IkSolver { index } => Some(index),
            _ => None,
        };
        let mut chips = h_flex().w_full().gap_2().p_2().flex_wrap().items_center();
        if rows.is_empty() {
            chips = chips.child(div().text_xs().text_color(theme.muted_foreground).child("No solvers. Select a chain and add a solver in the inspector. Solvers run left to right."));
        }
        for r in rows {
            let i = r.index;
            chips = chips.child(
                h_flex()
                    .id(SharedString::from(format!("solver-{i}")))
                    .gap_1()
                    .px_2()
                    .py_1()
                    .items_center()
                    .rounded(px(5.0))
                    .border_1()
                    .border_color(if selected == Some(i) { theme.primary } else { theme.border })
                    .bg(if selected == Some(i) { theme.accent } else { theme.secondary })
                    .cursor_pointer()
                    .text_xs()
                    .text_color(if r.enabled { theme.foreground } else { theme.muted_foreground })
                    .child(format!("{}. {} \u{b7} {} \u{2192} {}", i + 1, r.kind, r.chain, r.goal))
                    .when(!r.enabled, |c| c.child("(off)"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.select_solver(i);
                        cx.notify();
                    })),
            );
        }
        v_flex()
            .w_full()
            .h(px(92.0))
            .flex_shrink_0()
            .border_t_1()
            .border_color(theme.border)
            .child(div().px_3().h(px(24.0)).flex().items_center().text_xs().font_semibold().text_color(theme.muted_foreground).child("SOLVER ORDER"))
            .child(chips)
            .into_any_element()
    }

    fn bone_inspector(&mut self, bone: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let name = self.bone_name(bone);
        let theme = cx.theme().clone();
        let mut body = v_flex().w_full().gap_2().child(div().text_sm().font_semibold().text_color(theme.foreground).child(format!("Bone: {name}")));
        let start = self.chain_start.clone();
        let b = bone.to_string();
        let anchor_label = match &start {
            None => "Start a chain here".to_string(),
            Some(s) if s == bone => "Cancel chain".to_string(),
            Some(s) => format!("End chain here (from {})", self.bone_name(s)),
        };
        body = body.child(Button::new("ik-chain").icon(IconName::Link).label(anchor_label).small().outline().on_click(cx.listener(move |this, _, window, cx| {
            match this.chain_start.clone() {
                Some(s) if s == b => this.chain_start = None,
                Some(s) => {
                    let n = this.model.asset().chains.len() + 1;
                    let end = b.clone();
                    this.chain_start = None;
                    edit(this, window, cx, |m| m.add_chain(&format!("Chain_{n}"), &s, &end).map(|_| ()));
                }
                None => this.chain_start = Some(b.clone()),
            }
            cx.notify();
        })));
        let b2 = bone.to_string();
        body = body.child(Button::new("ik-goal").icon(IconName::Plus).label("Add goal on this bone").small().outline().on_click(cx.listener(move |this, _, window, cx| {
            let n = this.model.asset().goals.len() + 1;
            let b = b2.clone();
            let base = this.bone_name(&b).replace(|c: char| !c.is_ascii_alphanumeric(), "_");
            edit(this, window, cx, |m| m.add_goal(&format!("{base}_Goal{n}"), &b).map(|_| ()));
        })));
        let b3 = bone.to_string();
        body = body.child(Button::new("ik-limit").label("Limit this joint").small().outline().on_click(cx.listener(move |this, _, window, cx| {
            let b = b3.clone();
            edit(this, window, cx, |m| m.set_joint_limit(&b, [-45.0; 3], [45.0; 3]));
        })));
        let b4 = bone.to_string();
        let is_root = self.model.asset().retarget_root.as_deref() == Some(bone);
        body = body.child(Button::new("ik-root").label(if is_root { "Clear retarget root" } else { "Use as retarget root" }).small().outline().on_click(cx.listener(move |this, _, window, cx| {
            let b = b4.clone();
            edit(this, window, cx, |m| m.set_retarget_root(if is_root { None } else { Some(&b) }));
        })));
        let _ = window;
        body.into_any_element()
    }

    fn chain_inspector(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(chain) = self.model.asset().chain(name).cloned() else { return empty_inspector(&self.model.selection, "The chain no longer exists.", cx) };
        let mut body = v_flex().w_full().gap_2().child(div().text_sm().font_semibold().text_color(cx.theme().foreground).child("IK Chain"));
        body = body.child(self.widgets.text_field("chain.name", "Name", &chain.name, window, cx));
        body = body.child(value_row("Start bone", self.bone_name(&chain.start_bone), cx));
        body = body.child(value_row("End bone", self.bone_name(&chain.end_bone), cx));
        let (cid, sel) = (chain.id.clone(), self.selected_bone());
        body = body.child(
            Button::new("ik-chain-bones").label("Use selected bone as end").small().outline().disabled_when(sel.is_none()).on_click(cx.listener(move |this, _, window, cx| {
                let (cid, start) = (cid.clone(), this.model.asset().chains.iter().find(|c| c.id == cid).map(|c| c.start_bone.clone()));
                if let (Some(start), Some(end)) = (start, this.selected_bone()) {
                    edit(this, window, cx, |m| m.set_chain_bones(&cid, &start, &end));
                }
            })),
        );
        let goals: Vec<(String, String)> = self.model.asset().goals.iter().map(|g| (g.id.clone(), g.name.clone())).collect();
        let current = chain.goal.clone();
        let goal_label = current.as_ref().and_then(|g| goals.iter().find(|x| &x.0 == g)).map_or("None".to_string(), |g| g.1.clone());
        let cid = chain.id.clone();
        body = body.child(labelled(
            "Goal",
            Button::new("ik-chain-goal").label(goal_label).small().outline().tooltip("Cycle through the rig's goals").on_click(cx.listener(move |this, _, window, cx| {
                let next = match &current {
                    None => goals.first().map(|g| g.0.clone()),
                    Some(c) => goals.iter().position(|g| &g.0 == c).and_then(|p| goals.get(p + 1)).map(|g| g.0.clone()),
                };
                let cid = cid.clone();
                edit(this, window, cx, |m| m.set_chain_goal(&cid, next.as_deref()));
            })),
            cx,
        ));
        if let Some(goal) = chain.goal.clone() {
            let (a, b, cid) = (goal.clone(), goal, chain.id.clone());
            let cid2 = cid.clone();
            body = body.child(
                h_flex()
                    .gap_1()
                    .child(Button::new("ik-add-2b").label("Add Two-Bone solver").small().outline().on_click(cx.listener(move |this, _, window, cx| {
                        let (cid, a) = (cid.clone(), a.clone());
                        edit(this, window, cx, |m| m.add_solver(true, &cid, &a).map(|_| ()));
                    })))
                    .child(Button::new("ik-add-fabrik").label("Add FABRIK solver").small().outline().on_click(cx.listener(move |this, _, window, cx| {
                        let (cid, b) = (cid2.clone(), b.clone());
                        edit(this, window, cx, |m| m.add_solver(false, &cid, &b).map(|_| ()));
                    }))),
            );
        } else {
            body = body.child(div().text_xs().text_color(cx.theme().muted_foreground).child("Give the chain a goal to add a solver."));
        }
        let cid = chain.id.clone();
        body = body.child(Button::new("ik-chain-del").icon(IconName::Trash).label("Delete Chain").small().danger().on_click(cx.listener(move |this, _, window, cx| {
            let cid = cid.clone();
            edit(this, window, cx, |m| {
                m.delete_chain(&cid);
                Ok(())
            })
        })));
        body.into_any_element()
    }

    fn goal_inspector(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(goal) = self.model.asset().goals.iter().find(|g| g.name == name).cloned() else { return empty_inspector(&self.model.selection, "The goal no longer exists.", cx) };
        let mut body = v_flex().w_full().gap_2().child(div().text_sm().font_semibold().text_color(cx.theme().foreground).child("IK Goal"));
        body = body.child(self.widgets.text_field("goal.name", "Name", &goal.name, window, cx));
        body = body.child(value_row("Bone", self.bone_name(&goal.bone), cx));
        let gid = goal.id.clone();
        body = body.child(Button::new("ik-goal-bone").label("Move to selected bone").small().outline().on_click(cx.listener(move |this, _, window, cx| {
            let (gid, bone) = (gid.clone(), this.selected_bone());
            if let Some(b) = bone {
                edit(this, window, cx, |m| m.set_goal_bone(&gid, &b));
            }
        })));
        body = body.child(self.widgets.text_field("goal.pos", "Position alpha", &fmt_f32(goal.position_alpha), window, cx));
        body = body.child(self.widgets.text_field("goal.rot", "Rotation alpha", &fmt_f32(goal.rotation_alpha), window, cx));
        for (i, axis) in ["X", "Y", "Z"].iter().enumerate() {
            body = body.child(self.widgets.text_field(&format!("goal.off{i}"), &format!("Offset {axis}"), &fmt_f32(goal.default_offset[i]), window, cx));
        }
        let gid = goal.id.clone();
        body = body.child(Button::new("ik-goal-del").icon(IconName::Trash).label("Delete Goal").small().danger().on_click(cx.listener(move |this, _, window, cx| {
            let gid = gid.clone();
            edit(this, window, cx, |m| {
                m.delete_goal(&gid);
                Ok(())
            })
        })));
        body.into_any_element()
    }

    fn solver_inspector(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(solver) = self.model.asset().solvers.get(index).cloned() else { return empty_inspector(&self.model.selection, "The solver no longer exists.", cx) };
        let rows = self.model.solver_rows();
        let row = rows[index].clone();
        let theme = cx.theme().clone();
        let mut body = v_flex().w_full().gap_2().child(div().text_sm().font_semibold().text_color(theme.foreground).child(format!("{} solver", row.kind)));
        body = body.child(value_row("Order", format!("{} of {}", index + 1, rows.len()), cx));
        body = body.child(value_row("Chain", row.chain.clone(), cx));
        body = body.child(value_row("Goal", row.goal.clone(), cx));
        let enabled = solver.enabled;
        body = body.child(
            h_flex()
                .gap_1()
                .child(Button::new("ik-sv-up").icon(IconName::ArrowUp).label("Earlier").small().outline().on_click(cx.listener(move |this, _, window, cx| {
                    edit(this, window, cx, |m| {
                        m.move_solver(index, -1);
                        Ok(())
                    })
                })))
                .child(Button::new("ik-sv-down").icon(IconName::ArrowDown).label("Later").small().outline().on_click(cx.listener(move |this, _, window, cx| {
                    edit(this, window, cx, |m| {
                        m.move_solver(index, 1);
                        Ok(())
                    })
                })))
                .child(Button::new("ik-sv-en").label(if enabled { "Enabled" } else { "Disabled" }).small().outline().on_click(cx.listener(move |this, _, window, cx| {
                    edit(this, window, cx, |m| {
                        m.set_solver_enabled(index, !enabled);
                        Ok(())
                    })
                }))),
        );
        let toggle = |id: &'static str, label: &str, on: bool, cx: &mut Context<Self>, apply: fn(&mut SolverKind, bool)| {
            labelled(
                label,
                Button::new(id).label(if on { "On" } else { "Off" }).small().outline().on_click(cx.listener(move |this, _, window, cx| {
                    edit(this, window, cx, |m| m.edit_solver(index, |k| apply(k, !on)));
                })),
                cx,
            )
        };
        match &solver.kind {
            SolverKind::TwoBone { allow_stretch, max_stretch, take_goal_rotation, pole, .. } => {
                body = body.child(toggle("ik-2b-stretch", "Allow stretch", *allow_stretch, cx, |k, v| {
                    if let SolverKind::TwoBone { allow_stretch, .. } = k {
                        *allow_stretch = v;
                    }
                }));
                body = body.child(self.widgets.text_field("sv.stretch", "Max stretch", &fmt_f32(*max_stretch), window, cx));
                body = body.child(toggle("ik-2b-rot", "Take goal rotation", *take_goal_rotation, cx, |k, v| {
                    if let SolverKind::TwoBone { take_goal_rotation, .. } = k {
                        *take_goal_rotation = v;
                    }
                }));
                let pole_bone = pole.as_ref().and_then(|p| p.bone.clone());
                body = body.child(value_row("Pole bone", pole_bone.as_deref().map(|b| self.bone_name(b)).unwrap_or_else(|| "None".into()), cx));
                body = body.child(
                    h_flex()
                        .gap_1()
                        .child(Button::new("ik-pole-set").label("Use selected bone").small().outline().on_click(cx.listener(move |this, _, window, cx| {
                            if let Some(b) = this.selected_bone() {
                                edit(this, window, cx, |m| {
                                    m.edit_solver(index, |k| {
                                        if let SolverKind::TwoBone { pole, .. } = k {
                                            let mut p = pole.clone().unwrap_or(PoleSettings { bone: None, offset: [0.0; 3], space: Default::default() });
                                            p.bone = Some(b.clone());
                                            *pole = Some(p);
                                        }
                                    })
                                });
                            }
                        })))
                        .child(Button::new("ik-pole-clear").label("Clear").small().ghost().on_click(cx.listener(move |this, _, window, cx| {
                            edit(this, window, cx, |m| {
                                m.edit_solver(index, |k| {
                                    if let SolverKind::TwoBone { pole, .. } = k {
                                        *pole = None;
                                    }
                                })
                            })
                        }))),
                );
                let offset = pole.as_ref().map_or([0.0; 3], |p| p.offset);
                for (i, axis) in ["X", "Y", "Z"].iter().enumerate() {
                    body = body.child(self.widgets.text_field(&format!("sv.pole{i}"), &format!("Pole offset {axis}"), &fmt_f32(offset[i]), window, cx));
                }
            }
            SolverKind::Fabrik { precision, max_iterations, respect_joint_limits, take_goal_rotation, .. } => {
                body = body.child(self.widgets.text_field("sv.precision", "Precision", &fmt_f32(*precision), window, cx));
                body = body.child(self.widgets.text_field("sv.iterations", "Max iterations", &max_iterations.to_string(), window, cx));
                body = body.child(toggle("ik-fb-limits", "Respect joint limits", *respect_joint_limits, cx, |k, v| {
                    if let SolverKind::Fabrik { respect_joint_limits, .. } = k {
                        *respect_joint_limits = v;
                    }
                }));
                body = body.child(toggle("ik-fb-rot", "Take goal rotation", *take_goal_rotation, cx, |k, v| {
                    if let SolverKind::Fabrik { take_goal_rotation, .. } = k {
                        *take_goal_rotation = v;
                    }
                }));
            }
        }
        body = body.child(Button::new("ik-sv-del").icon(IconName::Trash).label("Delete Solver").small().danger().on_click(cx.listener(move |this, _, window, cx| {
            edit(this, window, cx, |m| {
                m.delete_solver(index);
                Ok(())
            })
        })));
        body.into_any_element()
    }

    fn limit_inspector(&mut self, bone: &str, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(l) = self.model.asset().joint_limits.iter().find(|l| l.bone == bone).cloned() else { return self.bone_inspector(bone, window, cx) };
        let mut body = v_flex().w_full().gap_2().child(div().text_sm().font_semibold().text_color(cx.theme().foreground).child(format!("Joint limit: {}", self.bone_name(bone))));
        for (i, axis) in ["X", "Y", "Z"].iter().enumerate() {
            body = body.child(self.widgets.text_field(&format!("limit.min{i}"), &format!("Min {axis} (deg)"), &fmt_f32(l.min_degrees[i]), window, cx));
            body = body.child(self.widgets.text_field(&format!("limit.max{i}"), &format!("Max {axis} (deg)"), &fmt_f32(l.max_degrees[i]), window, cx));
        }
        let b = bone.to_string();
        body = body.child(Button::new("ik-limit-del").icon(IconName::Trash).label("Remove Limit").small().danger().on_click(cx.listener(move |this, _, window, cx| {
            let b = b.clone();
            edit(this, window, cx, |m| {
                m.remove_joint_limit(&b);
                m.clear_selection();
                Ok(())
            })
        })));
        body.into_any_element()
    }
}

trait DisabledWhen: Sized {
    fn disabled_when(self, condition: bool) -> Self;
}
impl DisabledWhen for Button {
    fn disabled_when(self, condition: bool) -> Self {
        ui::Disableable::disabled(self, condition)
    }
}

impl EditorHost for IkRigEditor {
    fn widgets(&mut self) -> &mut Widgets {
        &mut self.widgets
    }

    fn commit_field(&mut self, key: &str, text: String, window: &mut Window, cx: &mut Context<Self>) {
        let v = parse_f32(&text);
        let sel = self.model.selection.clone();
        edit(self, window, cx, |m| {
            let n = |what: &str| v.ok_or_else(|| format!("{what} must be a number"));
            match (key, &sel) {
                ("name", _) => {
                    m.doc.apply("Rename", |a| a.name = text.trim().to_string());
                    Ok(())
                }
                ("chain.name", AnimSelection::IkChain { name }) => {
                    let id = m.asset().chain(name).map(|c| c.id.clone()).ok_or("no such chain")?;
                    m.rename_chain(&id, &text)
                }
                ("goal.name", AnimSelection::IkGoal { name }) => {
                    let id = m.asset().goals.iter().find(|g| &g.name == name).map(|g| g.id.clone()).ok_or("no such goal")?;
                    m.rename_goal(&id, &text)
                }
                (k, AnimSelection::IkGoal { name }) if k.starts_with("goal.") => {
                    let g = m.asset().goals.iter().find(|g| &g.name == name).cloned().ok_or("no such goal")?;
                    match k {
                        "goal.pos" => m.set_goal_alpha(&g.id, Some(n("alpha")?), None),
                        "goal.rot" => m.set_goal_alpha(&g.id, None, Some(n("alpha")?)),
                        _ => {
                            let axis = k.trim_start_matches("goal.off").parse::<usize>().map_err(|_| "unknown field".to_string())?;
                            let mut off = g.default_offset;
                            off[axis.min(2)] = n("offset")?;
                            m.set_goal_offset(&g.id, off)
                        }
                    }
                }
                (k, AnimSelection::IkSolver { index }) if k.starts_with("sv.") => {
                    let index = *index;
                    m.edit_solver(index, |kind| match (k, kind) {
                        ("sv.stretch", SolverKind::TwoBone { max_stretch, .. }) => *max_stretch = v.unwrap_or(*max_stretch),
                        ("sv.precision", SolverKind::Fabrik { precision, .. }) => *precision = v.unwrap_or(*precision),
                        ("sv.iterations", SolverKind::Fabrik { max_iterations, .. }) => *max_iterations = v.unwrap_or(*max_iterations as f32).max(0.0) as u32,
                        (p, SolverKind::TwoBone { pole, .. }) if p.starts_with("sv.pole") => {
                            let axis = p.trim_start_matches("sv.pole").parse::<usize>().unwrap_or(0).min(2);
                            let mut s = pole.clone().unwrap_or(PoleSettings { bone: None, offset: [0.0; 3], space: Default::default() });
                            s.offset[axis] = v.unwrap_or(s.offset[axis]);
                            *pole = Some(s);
                        }
                        _ => {}
                    })?;
                    n("value").map(|_| ())
                }
                (k, AnimSelection::JointLimit { bone }) if k.starts_with("limit.") => {
                    let l = m.asset().joint_limits.iter().find(|l| &l.bone == bone).cloned().ok_or("no such limit")?;
                    let (mut min, mut max) = (l.min_degrees, l.max_degrees);
                    let axis = k.chars().last().and_then(|c| c.to_digit(10)).unwrap_or(0) as usize;
                    if k.starts_with("limit.min") {
                        min[axis.min(2)] = n("limit")?;
                    } else {
                        max[axis.min(2)] = n("limit")?;
                    }
                    m.set_joint_limit(bone, min, max)
                }
                _ => Ok(()),
            }
        });
    }

    fn pick_ref(&mut self, key: &str, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if key == "skeleton" {
            let skeleton = id.as_deref().and_then(AssetRef::<SkeletonAssetKind>::parse);
            edit(self, window, cx, |m| {
                m.doc.apply("Set skeleton", |a| a.skeleton = skeleton);
                Ok(())
            });
            self.reload_skeleton();
        }
    }
}

impl EditorBody for IkRigEditor {
    type Model = IkRigModel;
    fn model(&self) -> &IkRigModel {
        &self.model
    }
    fn model_mut(&mut self) -> &mut IkRigModel {
        &mut self.model
    }
    fn chrome(&mut self) -> &mut Chrome {
        &mut self.chrome
    }
    fn chrome_ref(&self) -> &Chrome {
        &self.chrome
    }
    fn icon(&self) -> IconName {
        IconName::Link
    }
    fn kind_title(&self) -> &'static str {
        "IK Rig"
    }
    fn left_title(&self) -> &'static str {
        "Hierarchy"
    }

    fn select_location(&mut self, location: &str, _window: &mut Window, cx: &mut Context<Self>) {
        let a = self.model.asset().clone();
        if let Some(c) = a.chains.iter().find(|c| c.id == location) {
            self.model.select_chain(&c.name.clone());
        } else if let Some(g) = a.goals.iter().find(|g| g.id == location) {
            self.model.select_goal(&g.name.clone());
        } else if let Some(i) = a.solvers.iter().position(|s| s.id == location) {
            self.model.select_solver(i);
        } else {
            self.model.pick_bone(location);
        }
        cx.notify();
    }

    fn after_change(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        // a rig whose skeleton reference changed reloads it
        let id = self.model.asset().skeleton.as_ref().map(|s| s.as_str().to_string());
        let loaded = self.statuses.get("skeleton").map(|s| !matches!(s, RefStatus::Unset)).unwrap_or(false);
        if loaded != id.is_some() {
            self.reload_skeleton();
        }
    }

    fn toolbar_extra(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let hint = match &self.chain_start {
            Some(s) => format!("Pick the chain's end bone (started at {})", self.bone_name(s)),
            None => String::new(),
        };
        vec![div().text_xs().text_color(cx.theme().warning).child(hint).into_any_element()]
    }

    fn left(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let rows = self.model.bone_rows();
        let selected = self.selected_bone();
        let mut list = v_flex().w_full();
        if rows.is_empty() {
            list = list.child(div().p_3().text_xs().text_color(theme.muted_foreground).child("No skeleton loaded."));
        }
        for r in rows {
            let id = r.id.clone();
            let badges: Vec<String> = r.badges.chains.iter().map(|c| format!("\u{26d3} {c}")).chain(r.badges.goals.iter().map(|g| format!("\u{25ce} {g}"))).chain(r.badges.limited.then(|| "limit".to_string())).chain(r.badges.is_retarget_root.then(|| "root".to_string())).collect();
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("bone-{}", r.id)))
                    .w_full()
                    .pl(px(6.0 + r.depth as f32 * 10.0))
                    .pr_2()
                    .py_0p5()
                    .gap_1()
                    .items_center()
                    .cursor_pointer()
                    .when(selected.as_deref() == Some(r.id.as_str()), |d| d.bg(theme.accent))
                    .hover(|d| d.bg(theme.secondary))
                    .child(div().text_xs().text_color(theme.foreground).child(r.name))
                    .child(div().flex_1())
                    .child(div().text_xs().text_color(theme.muted_foreground).child(badges.join("  ")))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.pick_bone(&id);
                        cx.notify();
                    })),
            );
        }
        // chains and goals lists
        let a = self.model.asset().clone();
        list = list.child(div().px_2().pt_2().text_xs().font_semibold().text_color(theme.muted_foreground).child("CHAINS"));
        for (i, c) in a.chains.iter().enumerate() {
            let name = c.name.clone();
            let sel = matches!(&self.model.selection, AnimSelection::IkChain { name: n } if *n == c.name);
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("chain-{}", c.id)))
                    .w_full()
                    .px_2()
                    .py_0p5()
                    .gap_1()
                    .items_center()
                    .cursor_pointer()
                    .when(sel, |d| d.bg(theme.accent))
                    .hover(|d| d.bg(theme.secondary))
                    .child(div().size(px(8.0)).rounded_full().bg(chain_color(i)))
                    .child(div().text_xs().text_color(theme.foreground).child(c.name.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.select_chain(&name);
                        cx.notify();
                    })),
            );
        }
        list = list.child(div().px_2().pt_2().text_xs().font_semibold().text_color(theme.muted_foreground).child("GOALS"));
        for g in &a.goals {
            let name = g.name.clone();
            let sel = matches!(&self.model.selection, AnimSelection::IkGoal { name: n } if *n == g.name);
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("goal-{}", g.id)))
                    .w_full()
                    .px_2()
                    .py_0p5()
                    .gap_1()
                    .items_center()
                    .cursor_pointer()
                    .when(sel, |d| d.bg(theme.accent))
                    .hover(|d| d.bg(theme.secondary))
                    .child(Icon::new(IconName::Circle).size(px(10.0)).text_color(theme.success))
                    .child(div().text_xs().text_color(theme.foreground).child(g.name.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.select_goal(&name);
                        cx.notify();
                    })),
            );
        }
        list.into_any_element()
    }

    fn center(&mut self, cx_window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let _ = cx_window;
        let viewport = self.viewport(cx);
        let stack = self.solver_stack(cx);
        v_flex().size_full().child(div().flex_1().min_h_0().child(viewport)).child(stack).into_any_element()
    }

    fn inspector(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.model.selection.clone() {
            AnimSelection::IkChain { name } => self.chain_inspector(&name, window, cx),
            AnimSelection::IkGoal { name } => self.goal_inspector(&name, window, cx),
            AnimSelection::IkSolver { index } => self.solver_inspector(index, window, cx),
            AnimSelection::JointLimit { bone } => self.limit_inspector(&bone, window, cx),
            AnimSelection::Bone { id } => self.bone_inspector(&id, window, cx),
            _ => {
                let a = self.model.asset().clone();
                let status = self.statuses.get("skeleton").cloned().unwrap_or(RefStatus::Unset);
                let root = self.chrome.root.clone();
                let mut body = v_flex().w_full().gap_2().child(div().text_sm().font_semibold().text_color(cx.theme().foreground).child("IK Rig"));
                body = body.child(self.widgets.text_field("name", "Name", &a.name, window, cx));
                body = body.child(self.widgets.ref_field("skeleton", "Skeleton", SkeletonAssetKind::KIND, None, a.skeleton.as_ref().map(|s| s.as_str().to_string()), &status, &root, window, cx));
                body = body.child(value_row("Chains", a.chains.len().to_string(), cx));
                body = body.child(value_row("Goals", a.goals.len().to_string(), cx));
                body = body.child(value_row("Solvers", a.solvers.len().to_string(), cx));
                body = body.child(value_row("Retarget root", a.retarget_root.as_deref().map(|b| self.bone_name(b)).unwrap_or_else(|| "None".into()), cx));
                body = body.child(div().text_xs().text_color(cx.theme().muted_foreground).child("Click a bone in the viewport or the tree to start a chain, add a goal or limit a joint."));
                body.into_any_element()
            }
        }
    }
}

impl Render for IkRigEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        frame(self, window, cx)
    }
}

crate::editor_model!(IkRigModel, <pulsar_reflection::IkRigAssetKind as AssetKind>::KIND);
crate::anim_panel!(IkRigEditor, "ik-rig-editor");

