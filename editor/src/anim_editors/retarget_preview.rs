//! The animated preview of the Retarget Profile and IK Retargeter editors:
//! a shared sample Sequence is sampled on the source skeleton and pushed
//! through the real [`RetargetPlan`] (the one the runtime uses), then both
//! poses are projected for the side-by-side or overlay canvases. Nothing here
//! is saved; the sample, the playhead and the plan are editor state.

use std::sync::Arc;

use skeletal_assets::assets::AnimSeqAsset;
use skeletal_assets::diagnostics::Diagnostic;
use skeletal_assets::ik_retarget::IkRetargetAsset;
use skeletal_assets::ik_rig::IkRigAsset;
use skeletal_assets::retarget::RetargetProfileAsset;
use skeletal_assets::runtime::clip::CompiledClip;
use skeletal_assets::runtime::ikrig::CompiledIkRig;
use skeletal_assets::runtime::pose::{Pose, RuntimeSkeleton};
use skeletal_assets::runtime::retarget::RetargetPlan;
use skeletal_assets::skeleton::Skeleton;

use super::skeleton_proj::SkeletonProjection;

pub struct RetargetPreview {
    source: Arc<RuntimeSkeleton>,
    target: Arc<RuntimeSkeleton>,
    plan: RetargetPlan,
    clip: Option<CompiledClip>,
    source_bind: SkeletonProjection,
    target_bind: SkeletonProjection,
    pub diagnostics: Vec<Diagnostic>,
}

impl RetargetPreview {
    fn new(source_skeleton: &Skeleton, target_skeleton: &Skeleton) -> Result<(Arc<RuntimeSkeleton>, Arc<RuntimeSkeleton>), String> {
        let source = Arc::new(RuntimeSkeleton::new("preview-source", source_skeleton)?);
        let target = Arc::new(RuntimeSkeleton::new("preview-target", target_skeleton)?);
        Ok((source, target))
    }

    fn finish(source: Arc<RuntimeSkeleton>, target: Arc<RuntimeSkeleton>, plan: RetargetPlan, sample: Option<&AnimSeqAsset>, ss: &Skeleton, ts: &Skeleton) -> Self {
        let diagnostics = plan.diagnostics.clone();
        let clip = sample.map(|a| CompiledClip::compile(a, &source));
        Self { source_bind: SkeletonProjection::new(ss), target_bind: SkeletonProjection::new(ts), source, target, plan, clip, diagnostics }
    }

    /// Preview of a Retarget Profile. With both rigs the profile maps by chain.
    pub fn for_profile(
        asset: &RetargetProfileAsset,
        source_skeleton: &Skeleton,
        target_skeleton: &Skeleton,
        rigs: Option<(&IkRigAsset, &IkRigAsset)>,
        sample: Option<&AnimSeqAsset>,
    ) -> Result<Self, String> {
        let (source, target) = Self::new(source_skeleton, target_skeleton)?;
        let compiled = rigs.map(|(s, t)| (Arc::new(CompiledIkRig::compile(s, &source).0), Arc::new(CompiledIkRig::compile(t, &target).0)));
        let plan = RetargetPlan::from_profile(asset, source.clone(), target.clone(), compiled);
        Ok(Self::finish(source, target, plan, sample, source_skeleton, target_skeleton))
    }

    /// Preview of an IK Retargeter.
    pub fn for_ik_retargeter(
        asset: &IkRetargetAsset,
        source_skeleton: &Skeleton,
        target_skeleton: &Skeleton,
        source_rig: &IkRigAsset,
        target_rig: &IkRigAsset,
        sample: Option<&AnimSeqAsset>,
    ) -> Result<Self, String> {
        let (source, target) = Self::new(source_skeleton, target_skeleton)?;
        let sr = Arc::new(CompiledIkRig::compile(source_rig, &source).0);
        let tr = Arc::new(CompiledIkRig::compile(target_rig, &target).0);
        let plan = RetargetPlan::from_ik_retargeter(asset, source.clone(), target.clone(), sr, tr);
        Ok(Self::finish(source, target, plan, sample, source_skeleton, target_skeleton))
    }

    pub fn duration(&self) -> f32 {
        self.clip.as_ref().map_or(0.0, |c| c.duration)
    }

    pub fn has_sample(&self) -> bool {
        self.clip.is_some()
    }

    /// Source and target poses at `time` (the bind poses without a sample).
    pub fn poses_at(&self, time: f32) -> (Pose, Pose) {
        let source_pose = match &self.clip {
            Some(clip) => clip.sample(&self.source, time.clamp(0.0, clip.duration)),
            None => self.source.bind_pose(),
        };
        let target_pose = self.plan.retarget(&source_pose);
        (source_pose, target_pose)
    }

    /// Projections of both skeletons posed at `time`.
    pub fn projections_at(&self, time: f32) -> (SkeletonProjection, SkeletonProjection) {
        let (s, t) = self.poses_at(time);
        (self.source_bind.with_pose(&self.source, &s.bones), self.target_bind.with_pose(&self.target, &t.bones))
    }
}

#[cfg(test)]
mod tests {
    use super::super::ik_rig_model::tests::skeleton;
    use super::*;
    use skeletal_assets::retarget::{BoneMapping, RetargetProfileAsset, RotationMode, TranslationMode};

    fn mapped_profile(sk: &Skeleton) -> RetargetProfileAsset {
        let mut p = RetargetProfileAsset::new("P", None, None);
        for b in &sk.bones {
            p.bone_map.push(BoneMapping { id: format!("m-{}", b.id), source_bone: b.id.clone(), target_bone: b.id.clone(), rotation: RotationMode::OneToOne, translation: TranslationMode::None });
        }
        p
    }

    #[test]
    fn an_identity_mapping_reproduces_the_source_pose() {
        let sk = skeleton();
        let preview = RetargetPreview::for_profile(&mapped_profile(&sk), &sk, &sk, None, None).unwrap();
        assert!(!preview.has_sample() && preview.duration() == 0.0);
        let (s, t) = preview.projections_at(0.0);
        assert_eq!(s.bones.len(), t.bones.len());
        for (a, b) in s.bones.iter().zip(&t.bones) {
            for k in 0..3 {
                assert!((a.position[k] - b.position[k]).abs() < 1e-3, "{} differs on axis {k}", a.id);
            }
        }
    }

    #[test]
    fn a_changed_retarget_pose_changes_the_target_projection() {
        let sk = super::super::skeleton_proj::tests::leg();
        let mut profile = mapped_profile(&sk);
        let plain = RetargetPreview::for_profile(&profile, &sk, &sk, None, None).unwrap().projections_at(0.0).1;
        profile.target_pose.bone_rotations.push(skeletal_assets::retarget::BoneRotation { bone: "pelvis".into(), rotation: super::super::retarget_model::quat_from_euler_deg([0.0, 0.0, 40.0]) });
        let bent = RetargetPreview::for_profile(&profile, &sk, &sk, None, None).unwrap().projections_at(0.0).1;
        assert_ne!(plain, bent);
    }

    #[test]
    fn an_unmapped_profile_reports_through_the_plan_diagnostics() {
        let sk = skeleton();
        let profile = RetargetProfileAsset::new("Empty", None, None);
        let preview = RetargetPreview::for_profile(&profile, &sk, &sk, None, None).unwrap();
        assert!(!preview.diagnostics.is_empty());
    }
}
