//! IK Rig editor model: chains, goals, the ordered solver stack and joint
//! limits, authored over the rig's Skeleton.
//!
//! The skeleton is editor-resolved data (the view loads the referenced
//! `.skeleton`); with it the model checks that a chain is a contiguous run of
//! bones and produces the viewport overlay and hierarchy-tree rows.

use std::path::Path;
use std::sync::Arc;

use skeletal_assets::diagnostics::Diagnostic;
use skeletal_assets::ik_rig::{IkChain, IkGoal, IkRigAsset, IkSolver, JointLimit, PoleSettings, SolverKind};
use skeletal_assets::skeleton::Skeleton;

use super::document::AnimDocument;
use super::selection::AnimSelection;
use super::store::DocStore;

fn identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// What the hierarchy tree shows next to a bone.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BoneBadges {
    pub chains: Vec<String>,
    pub goals: Vec<String>,
    pub limited: bool,
    pub is_retarget_root: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BoneRow {
    pub id: String,
    pub name: String,
    pub depth: usize,
    pub badges: BoneBadges,
}

/// A chain drawn over the skeleton viewport.
#[derive(Clone, Debug, PartialEq)]
pub struct ChainOverlay {
    pub chain: String,
    pub name: String,
    pub bones: Vec<String>,
    pub goal: Option<String>,
    /// Position in the solver stack, if a solver drives this chain.
    pub solver_order: Option<usize>,
    pub valid: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SolverRow {
    pub index: usize,
    pub id: String,
    pub name: String,
    pub kind: &'static str,
    pub chain: String,
    pub goal: String,
    pub enabled: bool,
}

pub struct IkRigModel {
    pub doc: AnimDocument<IkRigAsset>,
    pub selection: AnimSelection,
    pub skeleton: Option<Skeleton>,
}

impl IkRigModel {
    pub fn open(store: Arc<dyn DocStore>, path: &Path, default_text: &str) -> Result<Self, String> {
        let doc = AnimDocument::<IkRigAsset>::open(store, path, default_text)?;
        Ok(Self { doc, selection: AnimSelection::None, skeleton: None })
    }

    pub fn asset(&self) -> &IkRigAsset {
        self.doc.value()
    }

    fn bone_known(&self, bone: &str) -> bool {
        self.skeleton.as_ref().map_or(true, |s| s.bone(bone).is_some())
    }

    pub fn chain_index(&self, id: &str) -> Option<usize> {
        self.asset().chains.iter().position(|c| c.id == id)
    }

    // ---- selection --------------------------------------------------------

    pub fn select_chain(&mut self, name: &str) -> bool {
        if self.asset().chain(name).is_some() {
            self.selection = AnimSelection::IkChain { name: name.into() };
            true
        } else {
            false
        }
    }
    pub fn select_goal(&mut self, name: &str) -> bool {
        if self.asset().goals.iter().any(|g| g.name == name) {
            self.selection = AnimSelection::IkGoal { name: name.into() };
            true
        } else {
            false
        }
    }
    pub fn select_solver(&mut self, index: usize) -> bool {
        if index < self.asset().solvers.len() {
            self.selection = AnimSelection::IkSolver { index };
            true
        } else {
            false
        }
    }
    pub fn select_bone(&mut self, bone: &str) {
        self.selection = AnimSelection::Bone { id: bone.into() };
    }

    /// Select whatever the bone is: a goal's bone selects the goal, a limited
    /// bone its limit, otherwise the bone itself. This is the viewport pick.
    pub fn pick_bone(&mut self, bone: &str) {
        if let Some(g) = self.asset().goals.iter().find(|g| g.bone == bone) {
            self.selection = AnimSelection::IkGoal { name: g.name.clone() };
        } else if self.asset().joint_limits.iter().any(|l| l.bone == bone) {
            self.selection = AnimSelection::JointLimit { bone: bone.into() };
        } else {
            self.selection = AnimSelection::Bone { id: bone.into() };
        }
    }

    // ---- chains -----------------------------------------------------------

    fn chain_error(&self, name: &str, skip: Option<usize>) -> Option<String> {
        if !identifier(name) {
            return Some("a chain name is letters, digits and underscore, not starting with a digit".into());
        }
        if self.asset().chains.iter().enumerate().any(|(i, c)| Some(i) != skip && c.name == name) {
            return Some(format!("a chain named `{name}` exists"));
        }
        None
    }

    /// The bones from `a` to `b` in chain order whichever was picked first, or
    /// an error when they are not on one branch.
    pub fn chain_between(&self, a: &str, b: &str) -> Result<(String, String), String> {
        let Some(sk) = &self.skeleton else { return Ok((a.to_string(), b.to_string())) };
        let probe = |s: &str, e: &str| {
            let chain = IkChain { id: String::new(), name: String::new(), start_bone: s.into(), end_bone: e.into(), goal: None };
            self.asset().chain_bones(sk, &chain).is_some()
        };
        if probe(a, b) {
            Ok((a.into(), b.into()))
        } else if probe(b, a) {
            Ok((b.into(), a.into()))
        } else {
            Err(format!("`{a}` and `{b}` are not on one branch of the skeleton"))
        }
    }

    /// Add a chain between two bones (in either pick order).
    pub fn add_chain(&mut self, name: &str, bone_a: &str, bone_b: &str) -> Result<String, String> {
        let name = name.trim();
        if let Some(error) = self.chain_error(name, None) {
            return Err(error);
        }
        if !self.bone_known(bone_a) || !self.bone_known(bone_b) {
            return Err("pick bones of the rig's skeleton".into());
        }
        let (start, end) = self.chain_between(bone_a, bone_b)?;
        let chain = IkChain { id: uuid::Uuid::new_v4().to_string(), name: name.to_string(), start_bone: start, end_bone: end, goal: None };
        let id = chain.id.clone();
        let sel = chain.name.clone();
        self.doc.apply("Add chain", |a| a.chains.push(chain));
        self.selection = AnimSelection::IkChain { name: sel };
        Ok(id)
    }

    pub fn rename_chain(&mut self, id: &str, name: &str) -> Result<(), String> {
        let i = self.chain_index(id).ok_or("no such chain")?;
        let name = name.trim();
        if let Some(error) = self.chain_error(name, Some(i)) {
            return Err(error);
        }
        let name = name.to_string();
        let old = self.asset().chains[i].name.clone();
        if matches!(&self.selection, AnimSelection::IkChain { name: n } if *n == old) {
            self.selection = AnimSelection::IkChain { name: name.clone() };
        }
        self.doc.edit("Rename chain", Some(&format!("cname:{id}")), |a| {
            a.chains[i].name = name;
            (true, ())
        });
        Ok(())
    }

    pub fn set_chain_bones(&mut self, id: &str, start: &str, end: &str) -> Result<(), String> {
        let i = self.chain_index(id).ok_or("no such chain")?;
        if !self.bone_known(start) || !self.bone_known(end) {
            return Err("pick bones of the rig's skeleton".into());
        }
        let (s, e) = self.chain_between(start, end)?;
        self.doc.apply("Set chain bones", |a| {
            a.chains[i].start_bone = s;
            a.chains[i].end_bone = e;
        });
        Ok(())
    }

    pub fn set_chain_goal(&mut self, id: &str, goal: Option<&str>) -> Result<(), String> {
        let i = self.chain_index(id).ok_or("no such chain")?;
        if let Some(g) = goal {
            if !self.asset().goals.iter().any(|x| x.id == g) {
                return Err("no such goal".into());
            }
        }
        let goal = goal.map(str::to_string);
        self.doc.edit("Set chain goal", None, |a| {
            let changed = a.chains[i].goal != goal;
            a.chains[i].goal = goal;
            (changed, ())
        });
        Ok(())
    }

    /// Delete a chain and the solvers that solve it.
    pub fn delete_chain(&mut self, id: &str) -> bool {
        let Some(i) = self.chain_index(id) else { return false };
        let name = self.asset().chains[i].name.clone();
        self.doc.apply("Delete chain", |a| {
            a.chains.remove(i);
            a.solvers.retain(|s| s.kind.chain() != id);
        });
        if matches!(&self.selection, AnimSelection::IkChain { name: n } if *n == name) {
            self.selection = AnimSelection::None;
        }
        true
    }

    // ---- goals --------------------------------------------------------------

    pub fn add_goal(&mut self, name: &str, bone: &str) -> Result<String, String> {
        let name = name.trim();
        if !identifier(name) {
            return Err("a goal name is letters, digits and underscore, not starting with a digit".into());
        }
        if self.asset().goals.iter().any(|g| g.name == name) {
            return Err(format!("a goal named `{name}` exists"));
        }
        if !self.bone_known(bone) {
            return Err("pick a bone of the rig's skeleton".into());
        }
        let goal = IkGoal { id: uuid::Uuid::new_v4().to_string(), name: name.into(), bone: bone.into(), position_alpha: 1.0, rotation_alpha: 1.0, default_offset: [0.0; 3] };
        let id = goal.id.clone();
        let sel = goal.name.clone();
        self.doc.apply("Add goal", |a| a.goals.push(goal));
        self.selection = AnimSelection::IkGoal { name: sel };
        Ok(id)
    }

    fn goal_index(&self, id: &str) -> Option<usize> {
        self.asset().goals.iter().position(|g| g.id == id)
    }

    pub fn rename_goal(&mut self, id: &str, name: &str) -> Result<(), String> {
        let i = self.goal_index(id).ok_or("no such goal")?;
        let name = name.trim();
        if !identifier(name) {
            return Err("a goal name is letters, digits and underscore, not starting with a digit".into());
        }
        if self.asset().goals.iter().enumerate().any(|(j, g)| j != i && g.name == name) {
            return Err(format!("a goal named `{name}` exists"));
        }
        let name = name.to_string();
        let old = self.asset().goals[i].name.clone();
        if matches!(&self.selection, AnimSelection::IkGoal { name: n } if *n == old) {
            self.selection = AnimSelection::IkGoal { name: name.clone() };
        }
        self.doc.edit("Rename goal", Some(&format!("gname:{id}")), |a| {
            a.goals[i].name = name;
            (true, ())
        });
        Ok(())
    }

    pub fn set_goal_alpha(&mut self, id: &str, position: Option<f32>, rotation: Option<f32>) -> Result<(), String> {
        let i = self.goal_index(id).ok_or("no such goal")?;
        for v in [position, rotation].into_iter().flatten() {
            if !v.is_finite() || !(0.0..=1.0).contains(&v) {
                return Err("alpha must be between 0 and 1".into());
            }
        }
        self.doc.edit("Set goal alpha", Some(&format!("galpha:{id}")), |a| {
            if let Some(p) = position {
                a.goals[i].position_alpha = p;
            }
            if let Some(r) = rotation {
                a.goals[i].rotation_alpha = r;
            }
            (true, ())
        });
        Ok(())
    }

    pub fn set_goal_offset(&mut self, id: &str, offset: [f32; 3]) -> Result<(), String> {
        let i = self.goal_index(id).ok_or("no such goal")?;
        if offset.iter().any(|v| !v.is_finite()) {
            return Err("offset must be numbers".into());
        }
        self.doc.edit("Set goal offset", Some(&format!("goff:{id}")), |a| {
            a.goals[i].default_offset = offset;
            (true, ())
        });
        Ok(())
    }

    pub fn set_goal_bone(&mut self, id: &str, bone: &str) -> Result<(), String> {
        let i = self.goal_index(id).ok_or("no such goal")?;
        if !self.bone_known(bone) {
            return Err("pick a bone of the rig's skeleton".into());
        }
        let bone = bone.to_string();
        self.doc.edit("Set goal bone", None, |a| {
            let changed = a.goals[i].bone != bone;
            a.goals[i].bone = bone;
            (changed, ())
        });
        Ok(())
    }

    /// Delete a goal, unlink it from chains and delete the solvers it drove.
    pub fn delete_goal(&mut self, id: &str) -> bool {
        let Some(i) = self.goal_index(id) else { return false };
        let name = self.asset().goals[i].name.clone();
        self.doc.apply("Delete goal", |a| {
            a.goals.remove(i);
            for c in &mut a.chains {
                if c.goal.as_deref() == Some(id) {
                    c.goal = None;
                }
            }
            a.solvers.retain(|s| s.kind.goal() != id);
        });
        if matches!(&self.selection, AnimSelection::IkGoal { name: n } if *n == name) {
            self.selection = AnimSelection::None;
        }
        true
    }

    // ---- solver stack ---------------------------------------------------------

    /// Append a solver. A two-bone solver needs a three-bone chain and a
    /// FABRIK solver at least two bones when the skeleton is known.
    pub fn add_solver(&mut self, two_bone: bool, chain: &str, goal: &str) -> Result<usize, String> {
        let ci = self.chain_index(chain).ok_or("no such chain")?;
        let g = self.asset().goals.iter().find(|g| g.id == goal).ok_or("no such goal")?.clone();
        if let Some(sk) = &self.skeleton {
            if let Some(bones) = self.asset().chain_bones(sk, &self.asset().chains[ci]) {
                if two_bone && bones.len() != 3 {
                    return Err(format!("a two-bone solver needs a 3-bone chain; this one has {}", bones.len()));
                }
                if !two_bone && bones.len() < 2 {
                    return Err("a FABRIK solver needs at least 2 bones".into());
                }
            }
        }
        let name = format!("{} {}", self.asset().chains[ci].name, if two_bone { "Two-Bone" } else { "FABRIK" });
        let kind = if two_bone {
            SolverKind::TwoBone { chain: chain.into(), goal: g.id, pole: None, allow_stretch: false, max_stretch: 1.25, take_goal_rotation: false }
        } else {
            SolverKind::Fabrik { chain: chain.into(), goal: g.id, precision: 0.001, max_iterations: 10, respect_joint_limits: true, take_goal_rotation: false }
        };
        let solver = IkSolver { id: uuid::Uuid::new_v4().to_string(), name, enabled: true, kind };
        let index = self.asset().solvers.len();
        self.doc.apply("Add solver", |a| a.solvers.push(solver));
        self.selection = AnimSelection::IkSolver { index };
        Ok(index)
    }

    /// Move a solver earlier (negative) or later (positive) in the stack.
    pub fn move_solver(&mut self, index: usize, delta: i32) -> bool {
        let len = self.asset().solvers.len();
        if index >= len {
            return false;
        }
        let to = (index as i32 + delta).clamp(0, len as i32 - 1) as usize;
        if to == index {
            return false;
        }
        self.doc.apply("Reorder solver", |a| {
            let s = a.solvers.remove(index);
            a.solvers.insert(to, s);
        });
        if matches!(self.selection, AnimSelection::IkSolver { index: i } if i == index) {
            self.selection = AnimSelection::IkSolver { index: to };
        }
        true
    }

    pub fn set_solver_enabled(&mut self, index: usize, enabled: bool) -> bool {
        if index >= self.asset().solvers.len() {
            return false;
        }
        self.doc.edit("Toggle solver", None, |a| {
            let changed = a.solvers[index].enabled != enabled;
            a.solvers[index].enabled = enabled;
            (changed, changed)
        })
    }

    pub fn delete_solver(&mut self, index: usize) -> bool {
        if index >= self.asset().solvers.len() {
            return false;
        }
        self.doc.apply("Delete solver", |a| {
            a.solvers.remove(index);
        });
        self.selection = AnimSelection::None;
        true
    }

    /// Edit the parameters of a solver. Invalid values are refused.
    pub fn edit_solver(&mut self, index: usize, edit: impl FnOnce(&mut SolverKind)) -> Result<(), String> {
        let mut candidate = self.asset().solvers.get(index).ok_or("no such solver")?.clone();
        edit(&mut candidate.kind);
        match &candidate.kind {
            SolverKind::TwoBone { max_stretch, .. } if !max_stretch.is_finite() || *max_stretch < 1.0 => return Err("max stretch must be at least 1".into()),
            SolverKind::Fabrik { precision, .. } if !precision.is_finite() || *precision <= 0.0 => return Err("precision must be positive".into()),
            SolverKind::Fabrik { max_iterations, .. } if *max_iterations == 0 || *max_iterations > 256 => return Err("iterations must be 1 to 256".into()),
            _ => {}
        }
        if let SolverKind::TwoBone { pole: Some(PoleSettings { bone: Some(b), .. }), .. } = &candidate.kind {
            if !self.bone_known(b) {
                return Err("the pole bone is not on the skeleton".into());
            }
        }
        if candidate.kind.chain() != self.asset().solvers[index].kind.chain() || candidate.kind.goal() != self.asset().solvers[index].kind.goal() {
            return Err("change the chain or goal by adding a new solver".into());
        }
        self.doc.edit("Edit solver", Some(&format!("solver:{index}")), |a| {
            a.solvers[index].kind = candidate.kind;
            (true, ())
        });
        Ok(())
    }

    pub fn solver_rows(&self) -> Vec<SolverRow> {
        let a = self.asset();
        a.solvers
            .iter()
            .enumerate()
            .map(|(index, s)| SolverRow {
                index,
                id: s.id.clone(),
                name: s.name.clone(),
                kind: match s.kind {
                    SolverKind::TwoBone { .. } => "Two-Bone",
                    SolverKind::Fabrik { .. } => "FABRIK",
                },
                chain: a.chains.iter().find(|c| c.id == s.kind.chain()).map_or("?".into(), |c| c.name.clone()),
                goal: a.goals.iter().find(|g| g.id == s.kind.goal()).map_or("?".into(), |g| g.name.clone()),
                enabled: s.enabled,
            })
            .collect()
    }

    // ---- joint limits ---------------------------------------------------------

    pub fn set_joint_limit(&mut self, bone: &str, min: [f32; 3], max: [f32; 3]) -> Result<(), String> {
        if !self.bone_known(bone) {
            return Err("pick a bone of the rig's skeleton".into());
        }
        for i in 0..3 {
            if !min[i].is_finite() || !max[i].is_finite() {
                return Err("limits must be numbers".into());
            }
            if min[i] > max[i] {
                return Err("each minimum must not exceed its maximum".into());
            }
        }
        let bone = bone.to_string();
        self.doc.edit("Set joint limit", Some(&format!("limit:{bone}")), |a| {
            match a.joint_limits.iter_mut().find(|l| l.bone == bone) {
                Some(l) => {
                    l.min_degrees = min;
                    l.max_degrees = max;
                }
                None => a.joint_limits.push(JointLimit { bone: bone.clone(), min_degrees: min, max_degrees: max }),
            }
            (true, ())
        });
        self.selection = AnimSelection::JointLimit { bone: bone.clone() };
        Ok(())
    }

    pub fn remove_joint_limit(&mut self, bone: &str) -> bool {
        if !self.asset().joint_limits.iter().any(|l| l.bone == bone) {
            return false;
        }
        self.doc.apply("Remove joint limit", |a| a.joint_limits.retain(|l| l.bone != bone));
        true
    }

    pub fn set_retarget_root(&mut self, bone: Option<&str>) -> Result<(), String> {
        if let Some(b) = bone {
            if !self.bone_known(b) {
                return Err("pick a bone of the rig's skeleton".into());
            }
        }
        let bone = bone.map(str::to_string);
        self.doc.edit("Set retarget root", None, |a| {
            let changed = a.retarget_root != bone;
            a.retarget_root = bone;
            (changed, ())
        });
        Ok(())
    }

    // ---- derived views ----------------------------------------------------------

    /// Chains for the viewport overlay, in rig order.
    pub fn chain_overlays(&self) -> Vec<ChainOverlay> {
        let a = self.asset();
        a.chains
            .iter()
            .map(|c| {
                let bones = self.skeleton.as_ref().and_then(|sk| a.chain_bones(sk, c));
                ChainOverlay {
                    chain: c.id.clone(),
                    name: c.name.clone(),
                    valid: bones.is_some() || self.skeleton.is_none(),
                    bones: bones.unwrap_or_else(|| vec![c.start_bone.clone(), c.end_bone.clone()]),
                    goal: c.goal.as_ref().and_then(|g| a.goals.iter().find(|x| &x.id == g)).map(|g| g.name.clone()),
                    solver_order: a.solvers.iter().position(|s| s.kind.chain() == c.id && s.enabled),
                }
            })
            .collect()
    }

    /// Depth-first bone rows with chain/goal/limit badges, for the hierarchy tree.
    pub fn bone_rows(&self) -> Vec<BoneRow> {
        let Some(sk) = &self.skeleton else { return Vec::new() };
        let overlays = self.chain_overlays();
        sk.depth_first()
            .into_iter()
            .map(|(bone, depth)| BoneRow {
                id: bone.id.clone(),
                name: bone.name.clone(),
                depth,
                badges: BoneBadges {
                    chains: overlays.iter().filter(|o| o.bones.contains(&bone.id)).map(|o| o.name.clone()).collect(),
                    goals: self.asset().goals.iter().filter(|g| g.bone == bone.id).map(|g| g.name.clone()).collect(),
                    limited: self.asset().joint_limits.iter().any(|l| l.bone == bone.id),
                    is_retarget_root: self.asset().retarget_root.as_deref() == Some(bone.id.as_str()),
                },
            })
            .collect()
    }

    pub fn validation(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        if let Err(message) = self.asset().validate_structure() {
            out.push(Diagnostic::error("ikrig.invalid", message));
        }
        out.extend(self.asset().reference_diagnostics());
        if let Some(sk) = &self.skeleton {
            out.extend(self.asset().diagnostics(sk));
        }
        out
    }

    pub fn save(&mut self) -> anyhow::Result<super::document::SaveOutcome> {
        self.doc.save()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::store::MemStore;
    use super::*;
    use skeletal_assets::skeleton::Bone;

    /// root > pelvis > thigh > shin > foot, and root > spine
    pub fn skeleton() -> Skeleton {
        let mut s = Skeleton::default();
        for (id, parent) in [("root", None), ("pelvis", Some("root")), ("thigh", Some("pelvis")), ("shin", Some("thigh")), ("foot", Some("shin")), ("spine", Some("root"))] {
            s.add_bone(Bone { id: id.into(), name: id.into(), parent: parent.map(Into::into), bind_transform: Default::default() }).unwrap();
        }
        s
    }

    fn model() -> IkRigModel {
        let store = MemStore::default();
        let mut m = IkRigModel::open(Arc::new(store), Path::new("r.ikrig"), &IkRigAsset::new("Rig", None).to_text().unwrap()).unwrap();
        m.skeleton = Some(skeleton());
        m
    }

    #[test]
    fn chains_are_contiguous_in_either_pick_order_and_named_uniquely() {
        let mut m = model();
        let id = m.add_chain("Leg", "foot", "thigh").unwrap();
        let c = &m.asset().chains[0];
        assert_eq!((c.start_bone.as_str(), c.end_bone.as_str()), ("thigh", "foot"), "ordered start to end");
        assert!(m.add_chain("Leg", "pelvis", "thigh").is_err());
        assert!(m.add_chain("1bad", "pelvis", "thigh").is_err());
        assert!(m.add_chain("Cross", "foot", "spine").is_err(), "not on one branch");
        assert!(m.add_chain("Ghost", "nope", "foot").is_err());
        m.rename_chain(&id, "LeftLeg").unwrap();
        assert!(m.set_chain_bones(&id, "spine", "foot").is_err());
        assert_eq!(m.selection, AnimSelection::IkChain { name: "LeftLeg".into() }, "selection follows a rename");
    }

    #[test]
    fn solvers_check_chain_length_reorder_and_stay_consistent_on_delete() {
        let mut m = model();
        let leg = m.add_chain("Leg", "thigh", "foot").unwrap(); // 3 bones
        let long = m.add_chain("Long", "pelvis", "foot").unwrap(); // 4 bones
        let goal = m.add_goal("FootGoal", "foot").unwrap();
        assert!(m.add_solver(true, &long, &goal).is_err(), "two-bone needs exactly 3 bones");
        let s0 = m.add_solver(true, &leg, &goal).unwrap();
        let s1 = m.add_solver(false, &long, &goal).unwrap();
        assert_eq!((s0, s1), (0, 1));
        assert!(m.move_solver(1, -1));
        assert_eq!(m.solver_rows()[0].kind, "FABRIK");
        assert_eq!(m.selection, AnimSelection::IkSolver { index: 0 });
        assert!(!m.move_solver(0, -1));
        assert!(m.set_solver_enabled(0, false));
        let overlays = m.chain_overlays();
        assert_eq!(overlays.iter().find(|o| o.name == "Long").unwrap().solver_order, None, "a disabled solver is not in the order");
        assert_eq!(overlays.iter().find(|o| o.name == "Leg").unwrap().solver_order, Some(1));
        assert_eq!(overlays[1].bones, ["pelvis", "thigh", "shin", "foot"]);
        m.delete_chain(&leg);
        assert_eq!(m.asset().solvers.len(), 1, "solvers of a deleted chain go with it");
        m.delete_goal(&goal);
        assert!(m.asset().solvers.is_empty());
        assert!(m.asset().validate_structure().is_ok());
    }

    #[test]
    fn solver_parameters_are_validated_and_chain_cannot_change() {
        let mut m = model();
        let leg = m.add_chain("Leg", "thigh", "foot").unwrap();
        let goal = m.add_goal("FootGoal", "foot").unwrap();
        m.add_solver(true, &leg, &goal).unwrap();
        assert!(m.edit_solver(0, |k| if let SolverKind::TwoBone { max_stretch, .. } = k { *max_stretch = 0.5 }).is_err());
        m.edit_solver(0, |k| {
            if let SolverKind::TwoBone { allow_stretch, max_stretch, pole, .. } = k {
                *allow_stretch = true;
                *max_stretch = 1.5;
                *pole = Some(PoleSettings { bone: Some("pelvis".into()), offset: [0.0, 1.0, 0.0], space: Default::default() });
            }
        })
        .unwrap();
        assert!(m.edit_solver(0, |k| if let SolverKind::TwoBone { pole, .. } = k { *pole = Some(PoleSettings { bone: Some("ghost".into()), offset: [0.0; 3], space: Default::default() }) }).is_err());
        assert!(m.edit_solver(0, |k| if let SolverKind::TwoBone { chain, .. } = k { *chain = "x".into() }).is_err());
        m.add_solver(false, &leg, &goal).unwrap();
        assert!(m.edit_solver(1, |k| if let SolverKind::Fabrik { max_iterations, .. } = k { *max_iterations = 0 }).is_err());
        assert!(m.edit_solver(1, |k| if let SolverKind::Fabrik { precision, .. } = k { *precision = 0.0 }).is_err());
        m.edit_solver(1, |k| if let SolverKind::Fabrik { max_iterations, .. } = k { *max_iterations = 32 }).unwrap();
    }

    #[test]
    fn goals_limits_and_the_viewport_pick() {
        let mut m = model();
        let g = m.add_goal("FootGoal", "foot").unwrap();
        assert!(m.add_goal("FootGoal", "shin").is_err());
        assert!(m.set_goal_alpha(&g, Some(1.5), None).is_err());
        m.set_goal_alpha(&g, Some(0.5), Some(0.25)).unwrap();
        m.set_goal_offset(&g, [0.0, 0.1, 0.0]).unwrap();
        assert!(m.set_goal_offset(&g, [f32::NAN, 0.0, 0.0]).is_err());
        m.set_joint_limit("shin", [0.0, 0.0, 0.0], [140.0, 0.0, 0.0]).unwrap();
        assert!(m.set_joint_limit("shin", [10.0, 0.0, 0.0], [5.0, 0.0, 0.0]).is_err());
        assert!(m.set_joint_limit("ghost", [0.0; 3], [1.0; 3]).is_err());
        m.pick_bone("foot");
        assert_eq!(m.selection, AnimSelection::IkGoal { name: "FootGoal".into() });
        m.pick_bone("shin");
        assert_eq!(m.selection, AnimSelection::JointLimit { bone: "shin".into() });
        m.pick_bone("spine");
        assert_eq!(m.selection, AnimSelection::Bone { id: "spine".into() });
        let rows = m.bone_rows();
        assert_eq!(rows.iter().find(|r| r.id == "shin").unwrap().badges.limited, true);
        assert_eq!(rows.iter().find(|r| r.id == "foot").unwrap().badges.goals, ["FootGoal"]);
        assert!(m.remove_joint_limit("shin") && !m.remove_joint_limit("shin"));
        m.set_retarget_root(Some("pelvis")).unwrap();
        assert!(m.bone_rows().iter().find(|r| r.id == "pelvis").unwrap().badges.is_retarget_root);
        assert!(m.set_retarget_root(Some("ghost")).is_err());
    }

    #[test]
    fn validation_surfaces_a_solverless_goal_and_save_round_trips() {
        let store = MemStore::default();
        let mut m = IkRigModel::open(Arc::new(store.clone()), Path::new("r.ikrig"), &IkRigAsset::new("Rig", None).to_text().unwrap()).unwrap();
        m.skeleton = Some(skeleton());
        let leg = m.add_chain("Leg", "thigh", "foot").unwrap();
        let goal = m.add_goal("FootGoal", "foot").unwrap();
        assert!(m.validation().iter().any(|d| d.code == "ikrig.unused_goal"));
        assert!(m.validation().iter().any(|d| d.code == "ikrig.no_skeleton"));
        m.set_chain_goal(&leg, Some(&goal)).unwrap();
        m.add_solver(true, &leg, &goal).unwrap();
        assert!(!m.validation().iter().any(|d| d.code == "ikrig.unused_goal"));
        m.set_joint_limit("shin", [0.0; 3], [120.0, 0.0, 0.0]).unwrap();
        m.save().unwrap();
        let r = IkRigModel::open(Arc::new(store), Path::new("r.ikrig"), "").unwrap();
        assert_eq!((r.asset().chains.len(), r.asset().goals.len(), r.asset().solvers.len(), r.asset().joint_limits.len()), (1, 1, 1, 1));
        assert_eq!(r.asset().chains[0].goal.as_deref(), Some(goal.as_str()));
    }
}
