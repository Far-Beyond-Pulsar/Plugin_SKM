//! The Animation Graph editor's live preview: the open graph compiled by the
//! real compiler and run by a real `AnimInstance`, on a transient instance that
//! lives only in the editor. Parameters are set the way a component sets them;
//! the pose, state-machine trace, curves and root motion are what the runtime
//! reports. Nothing here is written to the asset, and scrubbing uses the
//! runtime's `Scrub` mode so no gameplay events are produced.

use std::any::Any;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pulsar_reflection::{
    AnimationGraphAssetKind, AnimationMontageAssetKind, AnimationSequenceAssetKind, AssetKind, BlendSpaceAssetKind, IkRetargeterAssetKind, IkRigAssetKind, PoseDatabaseAssetKind,
    RetargetProfileAssetKind, SkeletonAssetKind,
};
use serde_json::{json, Value};
use skeletal_assets::anim_graph::catalogue::ParamType;
use skeletal_assets::anim_graph::AnimationGraphAsset;
use skeletal_assets::assets::{AnimSeqAsset, SkeletonAsset};
use skeletal_assets::blend_space::BlendSpaceAsset;
use skeletal_assets::diagnostics::{has_errors, Diagnostic};
use skeletal_assets::ik_retarget::IkRetargetAsset;
use skeletal_assets::ik_rig::IkRigAsset;
use skeletal_assets::montage::AnimationMontageAsset;
use skeletal_assets::pose_db::PoseDbAsset;
use skeletal_assets::retarget::RetargetProfileAsset;
use skeletal_assets::runtime::anim_instance::{AnimInstance, TimeStep, UpdateMode};
use skeletal_assets::runtime::graph::compile::{compile_graph, AssetSource};
use skeletal_assets::runtime::graph::Value as RuntimeValue;
use skeletal_assets::runtime::pose::RuntimeSkeleton;

use super::refs::ProjectRefIndex;
use super::skeleton_proj::SkeletonProjection;

/// Loads the assets a graph references from the project, caching each by id.
pub struct ProjectAssets {
    root: PathBuf,
    cache: Mutex<HashMap<(String, String), Option<Arc<dyn Any + Send + Sync>>>>,
}

impl ProjectAssets {
    pub fn new(root: &Path) -> Self {
        Self { root: root.to_path_buf(), cache: Mutex::new(HashMap::new()) }
    }

    fn load<T: Send + Sync + 'static>(&self, kind: &str, id: &str, parse: fn(&str) -> Option<T>) -> Option<Arc<T>> {
        let key = (kind.to_string(), id.to_string());
        if let Some(hit) = self.cache.lock().ok()?.get(&key) {
            return hit.clone().and_then(|a| a.downcast::<T>().ok());
        }
        let loaded = (|| {
            let path = ProjectRefIndex::new(&self.root).locate_path(kind, id)?;
            let text = String::from_utf8(engine_fs::virtual_fs::read_file(&path).ok()?).ok()?;
            parse(&text).map(|v| Arc::new(v) as Arc<dyn Any + Send + Sync>)
        })();
        self.cache.lock().ok()?.insert(key, loaded.clone());
        loaded.and_then(|a| a.downcast::<T>().ok())
    }
}

impl AssetSource for ProjectAssets {
    fn skeleton(&self, id: &str) -> Option<Arc<SkeletonAsset>> {
        self.load(SkeletonAssetKind::KIND, id, |t| SkeletonAsset::parse(t).ok())
    }
    fn sequence(&self, id: &str) -> Option<Arc<AnimSeqAsset>> {
        self.load(AnimationSequenceAssetKind::KIND, id, |t| AnimSeqAsset::parse(t).ok())
    }
    fn blend_space(&self, id: &str) -> Option<Arc<BlendSpaceAsset>> {
        self.load(BlendSpaceAssetKind::KIND, id, |t| BlendSpaceAsset::parse(t).ok())
    }
    fn montage(&self, id: &str) -> Option<Arc<AnimationMontageAsset>> {
        self.load(AnimationMontageAssetKind::KIND, id, |t| AnimationMontageAsset::parse(t).ok())
    }
    fn graph(&self, id: &str) -> Option<Arc<AnimationGraphAsset>> {
        self.load(AnimationGraphAssetKind::KIND, id, |t| AnimationGraphAsset::parse(t).ok())
    }
    fn ik_rig(&self, id: &str) -> Option<Arc<IkRigAsset>> {
        self.load(IkRigAssetKind::KIND, id, |t| IkRigAsset::parse(t).ok())
    }
    fn retarget_profile(&self, id: &str) -> Option<Arc<RetargetProfileAsset>> {
        self.load(RetargetProfileAssetKind::KIND, id, |t| RetargetProfileAsset::parse(t).ok())
    }
    fn ik_retargeter(&self, id: &str) -> Option<Arc<IkRetargetAsset>> {
        self.load(IkRetargeterAssetKind::KIND, id, |t| IkRetargetAsset::parse(t).ok())
    }
    fn pose_db(&self, id: &str) -> Option<Arc<PoseDbAsset>> {
        self.load(PoseDatabaseAssetKind::KIND, id, |t| PoseDbAsset::parse(t).ok())
    }
}

/// A snapshot of what the runtime reports after the latest advance.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PreviewReport {
    pub trace: Vec<String>,
    pub curves: Vec<(String, f32)>,
    pub root_motion_translation: [f32; 3],
    pub time: f32,
}

pub struct GraphPreview {
    instance: AnimInstance,
    projection: SkeletonProjection,
    runtime: Arc<RuntimeSkeleton>,
    pub time: f32,
}

impl GraphPreview {
    /// Compile `asset` against `source` and make a transient instance of it.
    pub fn build(asset: &AnimationGraphAsset, source: &dyn AssetSource) -> Result<Self, Vec<Diagnostic>> {
        let skeleton_id = asset.skeleton.as_ref().map(|s| s.as_str().to_string()).ok_or_else(|| vec![Diagnostic::error("preview.no_skeleton", "The graph has no skeleton").repair("Pick the graph's skeleton")])?;
        let skeleton = source.skeleton(&skeleton_id).ok_or_else(|| vec![Diagnostic::error("preview.skeleton_unavailable", "The graph's skeleton could not be loaded")])?.skeleton();
        let output = compile_graph(asset, source);
        let Some(graph) = output.graph else { return Err(output.diagnostics) };
        if has_errors(&output.diagnostics) {
            return Err(output.diagnostics);
        }
        let runtime = graph.skeleton.clone();
        let mut instance = AnimInstance::new(runtime.clone());
        instance.set_graph(graph).map_err(|e| vec![Diagnostic::error("preview.instance", e)])?;
        Ok(Self { projection: SkeletonProjection::new(&skeleton), instance, runtime, time: 0.0 })
    }

    pub fn set_parameter(&mut self, name: &str, value: Value) -> Result<(), String> {
        let ty = self.instance.parameters().find(|p| p.0 == name).map(|p| p.1).ok_or_else(|| format!("no parameter named {name}"))?;
        let bad = || format!("{name} is a {ty:?}");
        let runtime = match ty {
            ParamType::Float => RuntimeValue::Float(value.as_f64().ok_or_else(bad)? as f32),
            ParamType::Int => RuntimeValue::Int(value.as_i64().ok_or_else(bad)? as i32),
            ParamType::Bool => RuntimeValue::Bool(value.as_bool().ok_or_else(bad)?),
            ParamType::Vector => {
                let a = value.as_array().filter(|a| a.len() == 3).ok_or_else(bad)?;
                let n = |i: usize| a[i].as_f64().map(|v| v as f32).ok_or_else(bad);
                RuntimeValue::vector([n(0)?, n(1)?, n(2)?])
            }
        };
        self.instance.set_parameter(name, runtime).map_err(|e| format!("{e:?}"))
    }

    /// The preview's parameters with their current values.
    pub fn parameters(&self) -> Vec<(String, ParamType, Value)> {
        self.instance
            .parameters()
            .map(|(n, t, v)| {
                let json = match v {
                    RuntimeValue::Float(f) => json!(f),
                    RuntimeValue::Int(i) => json!(i),
                    RuntimeValue::Bool(b) => json!(b),
                    RuntimeValue::Vector(v) => json!([v.x, v.y, v.z]),
                    RuntimeValue::Transform(_) => Value::Null,
                };
                (n.to_string(), t, json)
            })
            .collect()
    }

    /// Run the instance forward. `scrub` seeks without producing gameplay events.
    pub fn advance(&mut self, dt: f32, scrub: bool) {
        self.instance.advance(dt, TimeStep::Variable, if scrub { UpdateMode::Scrub } else { UpdateMode::Play });
        self.time += dt;
    }

    /// The skeleton posed by the instance's latest pose.
    pub fn projection(&self) -> SkeletonProjection {
        self.projection.with_pose(&self.runtime, &self.instance.pose().bones)
    }

    pub fn report(&self) -> PreviewReport {
        let mut curves: Vec<(String, f32)> = self.instance.curves().iter().map(|(k, v)| (k.clone(), *v)).collect();
        curves.sort_by(|a, b| a.0.cmp(&b.0));
        let rm = self.instance.report.root_motion.translation;
        PreviewReport { trace: self.instance.report.trace.clone(), curves, root_motion_translation: [rm.x, rm.y, rm.z], time: self.time }
    }
}

#[cfg(test)]
mod tests {
    use super::super::anim_graph_model::AnimGraphModel;
    use super::super::ik_rig_model::tests::skeleton;
    use super::super::skeleton_proj::tests::leg;
    use super::super::store::MemStore;
    use super::*;
    use serde_json::json;
    use skeletal_assets::animation::AnimationClip;

    struct Mem {
        skeleton: Arc<SkeletonAsset>,
        sequence: Arc<AnimSeqAsset>,
    }

    impl AssetSource for Mem {
        fn skeleton(&self, id: &str) -> Option<Arc<SkeletonAsset>> {
            (self.skeleton.id.as_str() == id).then(|| self.skeleton.clone())
        }
        fn sequence(&self, id: &str) -> Option<Arc<AnimSeqAsset>> {
            (self.sequence.id.as_str() == id).then(|| self.sequence.clone())
        }
        fn blend_space(&self, _: &str) -> Option<Arc<BlendSpaceAsset>> {
            None
        }
        fn montage(&self, _: &str) -> Option<Arc<AnimationMontageAsset>> {
            None
        }
        fn graph(&self, _: &str) -> Option<Arc<AnimationGraphAsset>> {
            None
        }
        fn ik_rig(&self, _: &str) -> Option<Arc<IkRigAsset>> {
            None
        }
        fn retarget_profile(&self, _: &str) -> Option<Arc<RetargetProfileAsset>> {
            None
        }
        fn ik_retargeter(&self, _: &str) -> Option<Arc<IkRetargetAsset>> {
            None
        }
        fn pose_db(&self, _: &str) -> Option<Arc<PoseDbAsset>> {
            None
        }
    }

    fn fixture() -> (Mem, AnimationGraphAsset) {
        let sk = Arc::new(SkeletonAsset::new(leg()));
        let seq = Arc::new(AnimSeqAsset::new(Some(sk.id.clone()), AnimationClip::bind_pose()));
        let store = MemStore::default();
        let mut model = AnimGraphModel::open(Arc::new(store), Path::new("g.animgraph"), &AnimationGraphAsset::default().to_text().unwrap()).unwrap();
        model.doc.apply("skeleton", |a| a.skeleton = Some(sk.id.to_ref()));
        let player = model.add_node("anim.sequence_player", (0.0, 0.0)).unwrap();
        model.set_property(&player, "sequence", json!(seq.id.as_str())).unwrap();
        let out = model.graph().nodes.values().find(|n| n.node_type == "anim.output_pose").unwrap().id.clone();
        model.connect((&player, "pose"), (&out, "pose")).unwrap();
        let _ = skeleton();
        (Mem { skeleton: sk, sequence: seq }, model.asset().clone())
    }

    #[test]
    fn a_valid_graph_compiles_runs_and_reports_through_the_real_runtime() {
        let (source, asset) = fixture();
        let mut preview = GraphPreview::build(&asset, &source).expect("compiles");
        let before = preview.projection();
        preview.advance(0.1, false);
        preview.advance(0.1, true);
        assert!((preview.time - 0.2).abs() < 1e-5);
        assert_eq!(preview.projection().bones.len(), before.bones.len());
        assert!(preview.report().root_motion_translation.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn a_graph_that_does_not_compile_returns_its_diagnostics_instead_of_a_preview() {
        let (source, mut asset) = fixture();
        asset.skeleton = None;
        let err = GraphPreview::build(&asset, &source).err().expect("no skeleton, no preview");
        assert_eq!(err[0].code, "preview.no_skeleton");
        let (source, mut asset) = fixture();
        let out = asset.main.nodes.values().find(|n| n.node_type == "anim.output_pose").unwrap().id.clone();
        asset.main.connections.clear();
        for n in asset.main.nodes.values_mut() {
            for p in n.inputs.iter_mut().chain(n.outputs.iter_mut()) {
                p.pin.connected_to.clear();
            }
        }
        let _ = out;
        assert!(GraphPreview::build(&asset, &source).is_err(), "an Output Pose with nothing wired does not compile");
    }

    #[test]
    fn parameters_are_set_on_the_instance_and_validated() {
        let (source, mut asset) = fixture();
        asset.parameters.push(skeletal_assets::anim_graph::GraphParameter::new("Speed", ParamType::Float));
        let mut preview = GraphPreview::build(&asset, &source).expect("compiles");
        assert!(preview.set_parameter("Speed", json!(3.0)).is_ok());
        assert!(preview.set_parameter("Speed", json!("fast")).is_err());
        assert!(preview.set_parameter("Ghost", json!(1.0)).is_err());
        assert_eq!(preview.parameters().iter().find(|p| p.0 == "Speed").map(|p| p.2.clone()), Some(json!(3.0)));
    }
}

