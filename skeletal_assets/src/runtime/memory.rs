//! An in-memory [`AssetSource`](super::graph::compile::AssetSource): assets are
//! registered by id and handed out as shared `Arc`s. Tests, tools and
//! hosts that have already loaded their assets use it directly.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::anim_graph::AnimationGraphAsset;
use crate::assets::{AnimSeqAsset, SkeletonAsset};
use crate::blend_space::BlendSpaceAsset;
use crate::ik_retarget::IkRetargetAsset;
use crate::ik_rig::IkRigAsset;
use crate::montage::AnimationMontageAsset;
use crate::pose_db::PoseDbAsset;
use crate::retarget::RetargetProfileAsset;

use super::graph::compile::AssetSource;

#[derive(Default)]
pub struct MemorySource {
    pub skeletons: HashMap<String, Arc<SkeletonAsset>>,
    pub sequences: HashMap<String, Arc<AnimSeqAsset>>,
    pub blend_spaces: HashMap<String, Arc<BlendSpaceAsset>>,
    pub montages: HashMap<String, Arc<AnimationMontageAsset>>,
    pub graphs: HashMap<String, Arc<AnimationGraphAsset>>,
    pub ik_rigs: HashMap<String, Arc<IkRigAsset>>,
    pub profiles: HashMap<String, Arc<RetargetProfileAsset>>,
    pub retargeters: HashMap<String, Arc<IkRetargetAsset>>,
    pub pose_dbs: HashMap<String, Arc<PoseDbAsset>>,
    /// Derived pose indexes, by `(database id, key hash)`.
    pub indexes: Mutex<HashMap<(String, String), Vec<u8>>>,
}

impl MemorySource {
    pub fn index_builds(&self) -> usize {
        self.indexes.lock().map_or(0, |m| m.len())
    }
}

impl AssetSource for MemorySource {
    fn skeleton(&self, id: &str) -> Option<Arc<SkeletonAsset>> {
        self.skeletons.get(id).cloned()
    }
    fn sequence(&self, id: &str) -> Option<Arc<AnimSeqAsset>> {
        self.sequences.get(id).cloned()
    }
    fn blend_space(&self, id: &str) -> Option<Arc<BlendSpaceAsset>> {
        self.blend_spaces.get(id).cloned()
    }
    fn montage(&self, id: &str) -> Option<Arc<AnimationMontageAsset>> {
        self.montages.get(id).cloned()
    }
    fn graph(&self, id: &str) -> Option<Arc<AnimationGraphAsset>> {
        self.graphs.get(id).cloned()
    }
    fn ik_rig(&self, id: &str) -> Option<Arc<IkRigAsset>> {
        self.ik_rigs.get(id).cloned()
    }
    fn retarget_profile(&self, id: &str) -> Option<Arc<RetargetProfileAsset>> {
        self.profiles.get(id).cloned()
    }
    fn ik_retargeter(&self, id: &str) -> Option<Arc<IkRetargetAsset>> {
        self.retargeters.get(id).cloned()
    }
    fn pose_db(&self, id: &str) -> Option<Arc<PoseDbAsset>> {
        self.pose_dbs.get(id).cloned()
    }
    fn cached_pose_index(&self, database: &str, key: &str) -> Option<Vec<u8>> {
        self.indexes.lock().ok()?.get(&(database.to_string(), key.to_string())).cloned()
    }
    fn store_pose_index(&self, database: &str, key: &str, payload: &[u8]) {
        if let Ok(mut m) = self.indexes.lock() {
            m.insert((database.to_string(), key.to_string()), payload.to_vec());
        }
    }
}
