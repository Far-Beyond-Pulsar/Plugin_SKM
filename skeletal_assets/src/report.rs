//! The asset validator: everything that can be said about one animation asset
//! without opening a window.
//!
//! [`asset_report`] reads an asset with its codec, runs the asset's own
//! checks (against its skeleton or rigs when they resolve) and then checks the
//! dependency closure: missing, ambiguous or wrong-kind targets, incompatible
//! skeletons, cycles. The editors show this list; the importer and CI can
//! run it headlessly.

use crate::anim_graph::{validate, AnimationGraphAsset};
use crate::assets::{AnimSeqAsset, SkeletonAsset};
use crate::blend_space::BlendSpaceAsset;
use crate::dependencies::{check_closure, extract, AssetSource, Lookup};
use crate::diagnostics::Diagnostic;
use crate::ik_retarget::IkRetargetAsset;
use crate::ik_rig::IkRigAsset;
use crate::montage::AnimationMontageAsset;
use crate::pose_db::PoseDbAsset;
use crate::retarget::RetargetProfileAsset;
use crate::skeleton::Skeleton;
use pulsar_reflection::{
    AnimationGraphAssetKind, AnimationMontageAssetKind, AnimationSequenceAssetKind, AssetKind,
    AssetRef, BlendSpaceAssetKind, IkRetargeterAssetKind, IkRigAssetKind, PoseDatabaseAssetKind,
    RetargetProfileAssetKind, SkeletalMeshAssetKind, SkeletonAssetKind,
};

fn load_skeleton(source: &dyn AssetSource, id: &str) -> Option<Skeleton> {
    match source.read(SkeletonAssetKind::KIND, id) {
        Lookup::Found(bytes) => SkeletonAsset::parse(std::str::from_utf8(&bytes).ok()?).ok().map(|a| a.skeleton()),
        _ => None,
    }
}

fn load_rig(source: &dyn AssetSource, id: &str) -> Option<IkRigAsset> {
    match source.read(IkRigAssetKind::KIND, id) {
        Lookup::Found(bytes) => IkRigAsset::parse(std::str::from_utf8(&bytes).ok()?).ok(),
        _ => None,
    }
}

fn sequence_duration(source: &dyn AssetSource, id: &str) -> Option<f32> {
    match source.read(AnimationSequenceAssetKind::KIND, id) {
        Lookup::Found(bytes) => AnimSeqAsset::parse(std::str::from_utf8(&bytes).ok()?).ok().map(|a| a.clip.duration),
        _ => None,
    }
}

fn skeleton_of(source: &dyn AssetSource, reference: &Option<AssetRef<SkeletonAssetKind>>) -> Option<Skeleton> {
    load_skeleton(source, reference.as_ref()?.as_str())
}

/// The full report for the asset of `kind` stored in `bytes`.
pub fn asset_report(source: &dyn AssetSource, kind: &str, bytes: &[u8]) -> Vec<Diagnostic> {
    let extracted = match extract(kind, bytes) {
        Ok(e) => e,
        Err(message) => return vec![Diagnostic::error("asset.unreadable", message).repair("Fix or replace the file; it cannot be read with its format")],
    };
    let text = std::str::from_utf8(bytes).unwrap_or_default();
    let mut out = own_diagnostics(source, kind, text, bytes);
    out.extend(check_closure(source, kind, &extracted.id));
    out
}

fn own_diagnostics(source: &dyn AssetSource, kind: &str, text: &str, bytes: &[u8]) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    match kind {
        k if k == SkeletonAssetKind::KIND => {
            if let Ok(a) = SkeletonAsset::parse(text) {
                if let Err(e) = a.skeleton().validate() {
                    out.push(Diagnostic::error("skeleton.hierarchy", e.to_string()));
                }
            }
        }
        k if k == AnimationSequenceAssetKind::KIND => {
            if let Ok(a) = AnimSeqAsset::parse(text) {
                let skeleton = a.skeleton.as_ref().and_then(|s| load_skeleton(source, s.as_str()));
                if let Some(skeleton) = skeleton {
                    for track in &a.clip.tracks {
                        if skeleton.bone(&track.bone_id).is_none() {
                            out.push(Diagnostic::warning("animseq.unknown_bone", format!("Track for bone `{}` which the skeleton does not have; it is ignored", track.bone_id)).at(&track.bone_id));
                        }
                    }
                }
            }
        }
        k if k == SkeletalMeshAssetKind::KIND => {
            if let Err(e) = crate::skmesh::SkeletalMeshAsset::decode(bytes) {
                out.push(Diagnostic::error("skmesh.unreadable", e));
            }
        }
        k if k == BlendSpaceAssetKind::KIND => {
            if let Ok(a) = BlendSpaceAsset::parse(text) {
                out.extend(a.diagnostics());
                if a.skeleton.is_none() {
                    out.push(Diagnostic::warning("blendspace.no_skeleton", "No skeleton set").repair("Pick the blend space\u{2019}s skeleton"));
                }
            }
        }
        k if k == AnimationMontageAssetKind::KIND => {
            if let Ok(a) = AnimationMontageAsset::parse(text) {
                out.extend(a.diagnostics(|r| sequence_duration(source, r.as_str())));
                if let Some(skeleton) = skeleton_of(source, &a.skeleton) {
                    for track in &a.tracks {
                        if let Some(mask) = &track.mask {
                            for bone in mask.missing_bones(&skeleton) {
                                out.push(Diagnostic::error("montage.unknown_bone", format!("Slot `{}` mask names bone `{bone}` which the skeleton does not have", track.slot)).at(&track.id));
                            }
                        }
                    }
                }
            }
        }
        k if k == AnimationGraphAssetKind::KIND => {
            if let Ok(a) = AnimationGraphAsset::parse(text) {
                out.extend(validate::validate(&a));
                if let Some(skeleton) = skeleton_of(source, &a.skeleton) {
                    out.extend(validate::validate_against_skeleton(&a, &skeleton));
                }
            }
        }
        k if k == IkRigAssetKind::KIND => {
            if let Ok(a) = IkRigAsset::parse(text) {
                out.extend(a.reference_diagnostics());
                if let Some(skeleton) = skeleton_of(source, &a.skeleton) {
                    out.extend(a.diagnostics(&skeleton));
                }
            }
        }
        k if k == RetargetProfileAssetKind::KIND => {
            if let Ok(a) = RetargetProfileAsset::parse(text) {
                out.extend(a.reference_diagnostics());
                let source_skeleton = skeleton_of(source, &a.source_skeleton);
                let target_skeleton = skeleton_of(source, &a.target_skeleton);
                if let (Some(s), Some(t)) = (&source_skeleton, &target_skeleton) {
                    let rigs = match (&a.source_rig, &a.target_rig) {
                        (Some(sr), Some(tr)) => load_rig(source, sr.as_str()).zip(load_rig(source, tr.as_str())),
                        _ => None,
                    };
                    out.extend(a.diagnostics(s, t, rigs.as_ref().map(|(s, t)| (s, t))));
                }
            }
        }
        k if k == IkRetargeterAssetKind::KIND => {
            if let Ok(a) = IkRetargetAsset::parse(text) {
                out.extend(a.reference_diagnostics());
                if let (Some(sr), Some(tr)) = (&a.source_rig, &a.target_rig) {
                    if let (Some(s), Some(t)) = (load_rig(source, sr.as_str()), load_rig(source, tr.as_str())) {
                        out.extend(a.diagnostics(&s, &t));
                    }
                }
            }
        }
        k if k == PoseDatabaseAssetKind::KIND => {
            if let Ok(a) = PoseDbAsset::parse(text) {
                out.extend(a.reference_diagnostics());
                if let Some(skeleton) = skeleton_of(source, &a.skeleton) {
                    out.extend(a.diagnostics(&skeleton));
                }
            }
        }
        _ => {}
    }
    out
}
