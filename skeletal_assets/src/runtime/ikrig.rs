//! IK Rig execution: an `.ikrig` compiled against a runtime skeleton, run as an
//! ordered solver stack against goals.

use glam::{Quat, Vec3};

use crate::diagnostics::Diagnostic;
use crate::ik_rig::{IkRigAsset, PoleSettings, PoleSpace, SolverKind};

use super::ik::{solve_fabrik, solve_two_bone, Effector, FabrikParams, JointLimits, TwoBoneParams};
use super::pose::{slerp_short, BoneTransform, RuntimeSkeleton};

#[derive(Clone, Debug)]
pub struct RigChain {
    pub id: String,
    pub name: String,
    /// Start to end, as skeleton bone indices.
    pub bones: Vec<usize>,
    pub goal: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct RigGoal {
    pub id: String,
    pub name: String,
    pub bone: usize,
    pub position_alpha: f32,
    pub rotation_alpha: f32,
    pub default_offset: Vec3,
}

#[derive(Clone, Debug)]
pub enum RigSolverKind {
    TwoBone { chain: usize, goal: usize, pole: Option<ResolvedPole>, params: TwoBoneParams },
    Fabrik { chain: usize, goal: usize, params: FabrikParams },
}

#[derive(Clone, Debug)]
pub struct ResolvedPole {
    pub bone: Option<usize>,
    pub offset: Vec3,
    pub space: PoleSpace,
}

#[derive(Clone, Debug)]
pub struct RigSolver {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub kind: RigSolverKind,
}

#[derive(Clone, Debug)]
pub struct CompiledIkRig {
    pub id: String,
    pub chains: Vec<RigChain>,
    pub goals: Vec<RigGoal>,
    pub solvers: Vec<RigSolver>,
    pub limits: JointLimits,
    pub retarget_root: Option<usize>,
}

/// Goal transforms by goal index; `None` keeps the goal's authored default.
pub type GoalSet = Vec<Option<Effector>>;

/// What running a solver did, for the debugger and for tests.
#[derive(Clone, Debug, PartialEq)]
pub struct SolverOutcome {
    pub solver: String,
    pub ran: bool,
    pub detail: String,
}

impl CompiledIkRig {
    /// Resolve names to bone indices. Anything that cannot be resolved is
    /// reported and the solvers depending on it are dropped, never guessed.
    pub fn compile(asset: &IkRigAsset, skel: &RuntimeSkeleton) -> (Self, Vec<Diagnostic>) {
        let mut diagnostics = Vec::new();
        let bone = |id: &str| skel.bone(id);
        let mut chains = Vec::new();
        for c in &asset.chains {
            let (Some(start), Some(end)) = (bone(&c.start_bone), bone(&c.end_bone)) else {
                diagnostics.push(Diagnostic::error("ikrig.unknown_bone", format!("Chain `{}` uses a bone the skeleton does not have", c.name)).at(&c.id));
                chains.push(RigChain { id: c.id.clone(), name: c.name.clone(), bones: Vec::new(), goal: None });
                continue;
            };
            let Some(bones) = skel.chain(start, end) else {
                diagnostics.push(Diagnostic::error("ikrig.chain_shape", format!("Chain `{}` end is not below its start", c.name)).at(&c.id));
                chains.push(RigChain { id: c.id.clone(), name: c.name.clone(), bones: Vec::new(), goal: None });
                continue;
            };
            let goal = c.goal.as_ref().and_then(|g| asset.goals.iter().position(|x| &x.id == g));
            chains.push(RigChain { id: c.id.clone(), name: c.name.clone(), bones, goal });
        }
        let goals: Vec<RigGoal> = asset
            .goals
            .iter()
            .map(|g| RigGoal {
                id: g.id.clone(),
                name: g.name.clone(),
                bone: bone(&g.bone).unwrap_or(0),
                position_alpha: g.position_alpha,
                rotation_alpha: g.rotation_alpha,
                default_offset: Vec3::from_array(g.default_offset),
            })
            .collect();
        for (g, a) in goals.iter().zip(&asset.goals) {
            if bone(&a.bone).is_none() {
                diagnostics.push(Diagnostic::error("ikrig.unknown_bone", format!("Goal `{}` uses a bone the skeleton does not have", g.name)).at(&g.id));
            }
        }
        let mut solvers = Vec::new();
        for s in &asset.solvers {
            let chain = chains.iter().position(|c| c.id == s.kind.chain());
            let goal = goals.iter().position(|g| g.id == s.kind.goal());
            let (Some(chain), Some(goal)) = (chain, goal) else {
                diagnostics.push(Diagnostic::error("ikrig.solver_refs", format!("Solver `{}` names a chain or goal that does not exist", s.name)).at(&s.id));
                continue;
            };
            if chains[chain].bones.len() < 2 {
                diagnostics.push(Diagnostic::error("ikrig.chain_unresolved", format!("Solver `{}` needs chain `{}`, which does not resolve to bones", s.name, chains[chain].name)).at(&s.id).repair("Fix the chain's start and end bones"));
                continue;
            }
            let kind = match &s.kind {
                SolverKind::TwoBone { pole, allow_stretch, max_stretch, take_goal_rotation, .. } => {
                    if chains[chain].bones.len() < 3 {
                        diagnostics.push(Diagnostic::error("ikrig.two_bone_chain", format!("Two-bone solver `{}` needs a chain of at least three bones", s.name)).at(&s.id).repair("Extend the chain so it holds upper, middle and end bones"));
                        continue;
                    }
                    RigSolverKind::TwoBone {
                        chain,
                        goal,
                        pole: pole.as_ref().map(|p: &PoleSettings| ResolvedPole { bone: p.bone.as_deref().and_then(bone), offset: Vec3::from_array(p.offset), space: p.space }),
                        params: TwoBoneParams { alpha: 1.0, allow_stretch: *allow_stretch, max_stretch: *max_stretch, take_rotation: *take_goal_rotation },
                    }
                }
                SolverKind::Fabrik { precision, max_iterations, respect_joint_limits, take_goal_rotation, .. } => RigSolverKind::Fabrik {
                    chain,
                    goal,
                    params: FabrikParams { alpha: 1.0, precision: *precision, max_iterations: *max_iterations, take_rotation: *take_goal_rotation, respect_limits: *respect_joint_limits },
                },
            };
            solvers.push(RigSolver { id: s.id.clone(), name: s.name.clone(), enabled: s.enabled, kind });
        }
        let mut limits = JointLimits::default();
        for l in &asset.joint_limits {
            match bone(&l.bone) {
                Some(i) => {
                    limits.0.insert(i, (l.min_degrees, l.max_degrees));
                }
                None => diagnostics.push(Diagnostic::error("ikrig.unknown_bone", format!("Joint limit names bone `{}` which the skeleton does not have", l.bone)).at(&l.bone)),
            }
        }
        let retarget_root = asset.retarget_root.as_deref().and_then(bone);
        (Self { id: asset.id.as_str().to_string(), chains, goals, solvers, limits, retarget_root }, diagnostics)
    }

    pub fn goal_index(&self, name: &str) -> Option<usize> {
        self.goals.iter().position(|g| g.name == name)
    }

    pub fn chain_index(&self, name: &str) -> Option<usize> {
        self.chains.iter().position(|c| c.name == name)
    }

    /// Run the solver stack in order. Goals not in `goals` keep their authored
    /// default (the goal bone's current mesh transform plus the default offset).
    /// `alpha` blends the whole result with the input.
    pub fn execute(&self, skel: &RuntimeSkeleton, bones: &mut [BoneTransform], goals: &GoalSet, alpha: f32) -> Vec<SolverOutcome> {
        let input: Vec<BoneTransform> = bones.to_vec();
        let mut outcomes = Vec::new();
        for solver in &self.solvers {
            if !solver.enabled {
                outcomes.push(SolverOutcome { solver: solver.name.clone(), ran: false, detail: "disabled".into() });
                continue;
            }
            let (chain_idx, goal_idx) = match &solver.kind {
                RigSolverKind::TwoBone { chain, goal, .. } | RigSolverKind::Fabrik { chain, goal, .. } => (*chain, *goal),
            };
            let goal = &self.goals[goal_idx];
            let mesh = skel.bone_mesh_space(bones, goal.bone);
            let (_, cur_rot, cur_pos) = mesh.to_scale_rotation_translation();
            let supplied = goals.get(goal_idx).copied().flatten();
            let target = supplied.unwrap_or(Effector { translation: cur_pos + goal.default_offset, rotation: cur_rot });
            let effector = Effector {
                translation: cur_pos.lerp(target.translation, goal.position_alpha),
                rotation: slerp_short(cur_rot, target.rotation, goal.rotation_alpha),
            };
            let chain = &self.chains[chain_idx];
            let detail = match &solver.kind {
                RigSolverKind::TwoBone { pole, params, .. } => {
                    let end = *chain.bones.last().expect("chain checked at compile");
                    let pole_dir = pole.as_ref().and_then(|p| {
                        let upper = super::ik::two_bone_limb(skel, end)?.0;
                        let upper_pos = skel.bone_mesh_space(bones, upper).to_scale_rotation_translation().2;
                        Some(match p.bone {
                            Some(b) => skel.bone_mesh_space(bones, b).to_scale_rotation_translation().2 + p.offset - upper_pos,
                            None if p.space == PoleSpace::Goal => effector.rotation * p.offset,
                            None => p.offset,
                        })
                    });
                    match solve_two_bone(skel, bones, end, &effector, pole_dir, params, &self.limits) {
                        Some(r) => format!("two-bone stretch {:.3}{}", r.stretch, if r.out_of_reach { " out of reach" } else { "" }),
                        None => "no limb".into(),
                    }
                }
                RigSolverKind::Fabrik { params, .. } => match solve_fabrik(skel, bones, &chain.bones, &effector, params, &self.limits) {
                    Some(r) => format!("fabrik {} iterations, tip error {:.4}", r.iterations, r.tip_error),
                    None => "chain too short".into(),
                },
            };
            outcomes.push(SolverOutcome { solver: solver.name.clone(), ran: true, detail });
        }
        let alpha = alpha.clamp(0.0, 1.0);
        if alpha < 1.0 {
            for (b, i) in bones.iter_mut().zip(&input) {
                *b = i.blend(b, alpha);
            }
        }
        let _ = Quat::IDENTITY;
        outcomes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ik_rig::{IkChain, IkGoal, IkSolver};
    use crate::runtime::ik::tests::limb;

    pub(crate) fn rig(skel: &RuntimeSkeleton, fabrik: bool) -> CompiledIkRig {
        let mut asset = IkRigAsset::new("r", None);
        asset.chains.push(IkChain { id: "c".into(), name: "arm".into(), start_bone: "b0".into(), end_bone: "b2".into(), goal: Some("g".into()) });
        asset.goals.push(IkGoal { id: "g".into(), name: "hand".into(), bone: "b2".into(), position_alpha: 1.0, rotation_alpha: 1.0, default_offset: [0.0; 3] });
        asset.solvers.push(IkSolver {
            id: "s".into(),
            name: "solve".into(),
            enabled: true,
            kind: if fabrik {
                SolverKind::Fabrik { chain: "c".into(), goal: "g".into(), precision: 1e-4, max_iterations: 30, respect_joint_limits: true, take_goal_rotation: false }
            } else {
                SolverKind::TwoBone { chain: "c".into(), goal: "g".into(), pole: Some(PoleSettings { bone: None, offset: [1.0, 0.0, 0.0], space: PoleSpace::Mesh }), allow_stretch: false, max_stretch: 1.25, take_goal_rotation: false }
            },
        });
        asset.validate_structure().unwrap();
        let (rig, d) = CompiledIkRig::compile(&asset, skel);
        assert!(d.is_empty(), "{d:?}");
        rig
    }

    #[test]
    fn the_stack_moves_the_goal_bone_and_default_goals_hold_the_pose() {
        for fabrik in [false, true] {
            let skel = limb(3, 1.0);
            let rig = rig(&skel, fabrik);
            let mut bones = skel.bind.clone();
            // unconnected goal: stays where it is
            let out = rig.execute(&skel, &mut bones, &vec![None], 1.0);
            assert!(out[0].ran);
            let tip = skel.bone_mesh_space(&bones, 2).to_scale_rotation_translation().2;
            assert!((tip - Vec3::new(0.0, 2.0, 0.0)).length() < 1e-2, "fabrik={fabrik} {tip}");
            // supplied goal: reached
            let goal = Vec3::new(1.0, 1.0, 0.0);
            let mut bones = skel.bind.clone();
            rig.execute(&skel, &mut bones, &vec![Some(Effector::at(goal))], 1.0);
            let tip = skel.bone_mesh_space(&bones, 2).to_scale_rotation_translation().2;
            assert!((tip - goal).length() < 1e-2, "fabrik={fabrik} {tip}");
            // alpha 0: the input pose
            let mut none = skel.bind.clone();
            rig.execute(&skel, &mut none, &vec![Some(Effector::at(goal))], 0.0);
            assert_eq!(none, skel.bind);
        }
    }

    #[test]
    fn a_goal_alpha_of_half_stops_halfway() {
        let skel = limb(3, 1.0);
        let mut asset = IkRigAsset::new("r", None);
        asset.chains.push(IkChain { id: "c".into(), name: "arm".into(), start_bone: "b0".into(), end_bone: "b2".into(), goal: Some("g".into()) });
        asset.goals.push(IkGoal { id: "g".into(), name: "hand".into(), bone: "b2".into(), position_alpha: 0.5, rotation_alpha: 1.0, default_offset: [0.0; 3] });
        asset.solvers.push(IkSolver { id: "s".into(), name: "s".into(), enabled: true, kind: SolverKind::Fabrik { chain: "c".into(), goal: "g".into(), precision: 1e-4, max_iterations: 30, respect_joint_limits: true, take_goal_rotation: false } });
        let (rig, _) = CompiledIkRig::compile(&asset, &skel);
        let mut bones = skel.bind.clone();
        rig.execute(&skel, &mut bones, &vec![Some(Effector::at(Vec3::new(1.0, 1.0, 0.0)))], 1.0);
        let tip = skel.bone_mesh_space(&bones, 2).to_scale_rotation_translation().2;
        assert!((tip - Vec3::new(0.5, 1.5, 0.0)).length() < 2e-2, "{tip}");
    }

    #[test]
    fn unresolved_references_are_reported_and_the_solver_is_dropped() {
        let skel = limb(3, 1.0);
        let mut asset = IkRigAsset::new("r", None);
        asset.chains.push(IkChain { id: "c".into(), name: "arm".into(), start_bone: "b0".into(), end_bone: "nope".into(), goal: Some("g".into()) });
        asset.goals.push(IkGoal { id: "g".into(), name: "hand".into(), bone: "b2".into(), position_alpha: 1.0, rotation_alpha: 1.0, default_offset: [0.0; 3] });
        asset.solvers.push(IkSolver { id: "s".into(), name: "s".into(), enabled: true, kind: SolverKind::Fabrik { chain: "c".into(), goal: "g".into(), precision: 1e-3, max_iterations: 10, respect_joint_limits: true, take_goal_rotation: false } });
        let (rig, d) = CompiledIkRig::compile(&asset, &skel);
        assert!(rig.solvers.is_empty());
        assert!(d.iter().any(|d| d.code == "ikrig.unknown_bone") && d.iter().any(|d| d.code == "ikrig.chain_unresolved"), "{d:?}");
    }
}
