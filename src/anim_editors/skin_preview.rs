//! The animated, skinned preview of the Montage and Blend Space editors.
//!
//! The pose is not sampled by the editor: it comes from a transient
//! [`AnimInstance`] on the real runtime, the way a skeletal mesh component runs
//! one. A Montage is compiled and played on a Slot over the reference pose; a
//! Blend Space is played by a Blend Space Player fed by two parameters. The
//! instance lives only in the editor; unsaved edits are served to the compiler
//! through an overlay over the project assets, so the preview shows what is on
//! screen, and every advance uses `UpdateMode::Scrub`, so scrubbing and playing
//! the preview never produces a gameplay event.
//!
//! The posed skeleton goes to the viewport's own scene builder and renderer,
//! drawn offscreen, so the mesh is skinned exactly as in the Sequence editor.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::json;
use skeletal_assets::anim_graph::catalogue::ParamType;
use skeletal_assets::anim_graph::{data_connection, instantiate_node, refresh_node_pins, AnimationGraphAsset};
use skeletal_assets::assets::{AnimSeqAsset, SkeletonAsset};
use skeletal_assets::blend_space::BlendSpaceAsset;
use skeletal_assets::ik_retarget::IkRetargetAsset;
use skeletal_assets::ik_rig::IkRigAsset;
use skeletal_assets::montage::AnimationMontageAsset;
use skeletal_assets::pose_db::PoseDbAsset;
use skeletal_assets::retarget::RetargetProfileAsset;
use skeletal_assets::runtime::anim_instance::{AnimInstance, TimeStep, UpdateMode};
use skeletal_assets::runtime::clip::CompiledClip;
use skeletal_assets::runtime::graph::compile::{compile_graph, AssetSource};
use skeletal_assets::runtime::montage::CompiledMontage;
use skeletal_assets::runtime::pose::{Pose, RuntimeSkeleton};
use skeletal_assets::diagnostics::{has_errors, Diagnostic};
use skeletal_assets::skeleton::Skeleton;
use skeletal_assets::skmesh::SkeletalMeshAsset;

use crate::core::{Mat4, Vec3};

use super::graph_preview::ProjectAssets;

/// A skeleton, its runtime form and a mesh to skin with it.
pub struct Rig {
    pub skeleton_ref: pulsar_reflection::AssetRef<pulsar_reflection::SkeletonAssetKind>,
    pub skeleton: Skeleton,
    pub runtime: Arc<RuntimeSkeleton>,
    pub mesh: Option<SkeletalMeshAsset>,
}

impl Rig {
    pub fn new(asset: &SkeletonAsset, mesh: Option<SkeletalMeshAsset>) -> Result<Self, String> {
        let skeleton = asset.skeleton();
        let runtime = Arc::new(RuntimeSkeleton::new(asset.id.as_str(), &skeleton)?);
        Ok(Self { skeleton_ref: asset.id.to_ref(), skeleton, runtime, mesh })
    }
}

/// The project assets with the document being edited standing in for its saved
/// copy, so a preview of unsaved work compiles what the editor shows.
pub struct OverlaySource<'a> {
    pub base: &'a dyn AssetSource,
    pub blend_space: Option<Arc<BlendSpaceAsset>>,
    pub montage: Option<Arc<AnimationMontageAsset>>,
}

impl AssetSource for OverlaySource<'_> {
    fn skeleton(&self, id: &str) -> Option<Arc<SkeletonAsset>> {
        self.base.skeleton(id)
    }
    fn sequence(&self, id: &str) -> Option<Arc<AnimSeqAsset>> {
        self.base.sequence(id)
    }
    fn blend_space(&self, id: &str) -> Option<Arc<BlendSpaceAsset>> {
        match &self.blend_space {
            Some(own) if own.id.as_str() == id => Some(own.clone()),
            _ => self.base.blend_space(id),
        }
    }
    fn montage(&self, id: &str) -> Option<Arc<AnimationMontageAsset>> {
        match &self.montage {
            Some(own) if own.id.as_str() == id => Some(own.clone()),
            _ => self.base.montage(id),
        }
    }
    fn graph(&self, id: &str) -> Option<Arc<AnimationGraphAsset>> {
        self.base.graph(id)
    }
    fn ik_rig(&self, id: &str) -> Option<Arc<IkRigAsset>> {
        self.base.ik_rig(id)
    }
    fn retarget_profile(&self, id: &str) -> Option<Arc<RetargetProfileAsset>> {
        self.base.retarget_profile(id)
    }
    fn ik_retargeter(&self, id: &str) -> Option<Arc<IkRetargetAsset>> {
        self.base.ik_retargeter(id)
    }
    fn pose_db(&self, id: &str) -> Option<Arc<PoseDbAsset>> {
        self.base.pose_db(id)
    }
}

fn add(asset: &mut AnimationGraphAsset, id: &str, ty: &str, x: f32, props: &[(&str, serde_json::Value)]) -> Result<(), String> {
    let params: Vec<(String, ParamType)> = asset.parameters.iter().map(|p| (p.name.clone(), p.ty)).collect();
    let lookup = |name: &str| params.iter().find(|p| p.0 == name).map(|p| p.1);
    let mut node = instantiate_node(id, ty, (x, 0.0), &lookup).ok_or_else(|| format!("unknown node type {ty}"))?;
    for (key, value) in props {
        node.properties.insert((*key).to_string(), value.clone());
    }
    let lost = refresh_node_pins(&mut node, &lookup);
    debug_assert!(lost.is_empty());
    asset.main.add_node(node)
}

fn wire(asset: &mut AnimationGraphAsset, from: (&str, &str), to: (&str, &str)) -> Result<(), String> {
    asset.main.add_connection(data_connection(from.0, from.1, to.0, to.1))
}

fn finish(asset: &AnimationGraphAsset, source: &dyn AssetSource) -> Result<AnimInstance, Vec<Diagnostic>> {
    let output = compile_graph(asset, source);
    let Some(graph) = output.graph else { return Err(output.diagnostics) };
    if has_errors(&output.diagnostics) {
        return Err(output.diagnostics);
    }
    let mut instance = AnimInstance::new(graph.skeleton.clone());
    instance.set_graph(graph).map_err(|e| vec![Diagnostic::error("preview.instance", e)])?;
    Ok(instance)
}

/// A Montage played on a transient instance.
pub struct MontagePreview {
    instance: AnimInstance,
    montage: Arc<CompiledMontage>,
    position: f32,
    pub length: f32,
}

impl MontagePreview {
    /// A graph with one Slot per slot the Montage uses, chained over the
    /// reference pose, with the Montage compiled against the same skeleton.
    pub fn build(asset: &AnimationMontageAsset, rig: &Rig, source: &dyn AssetSource) -> Result<Self, Vec<Diagnostic>> {
        let err = |code: &'static str, message: String| vec![Diagnostic::error(code, message)];
        let mut slots: Vec<(String, String)> = Vec::new();
        for t in &asset.tracks {
            if !slots.iter().any(|s| s.0 == t.slot && s.1 == t.group) {
                slots.push((t.slot.clone(), t.group.clone()));
            }
        }
        if slots.is_empty() {
            return Err(err("preview.no_tracks", "The Montage has no track to play".into()));
        }
        let mut graph = AnimationGraphAsset::default();
        let step = |r: Result<(), String>| r.map_err(|m| err("preview.graph", m));
        step(add(&mut graph, "ref", "anim.reference_pose", 0.0, &[]))?;
        let mut previous = ("ref".to_string(), "pose".to_string());
        for (i, (slot, group)) in slots.iter().enumerate() {
            let id = format!("slot{i}");
            step(add(&mut graph, &id, "anim.slot", 100.0 + 100.0 * i as f32, &[("slot", json!(slot)), ("group", json!(group))]))?;
            step(wire(&mut graph, (&previous.0, &previous.1), (&id, "source")))?;
            previous = (id, "pose".to_string());
        }
        step(wire(&mut graph, (&previous.0, &previous.1), ("output", "pose")))?;
        let mut instance = finish_with_skeleton(&graph, rig, source)?;

        let mut cache: HashMap<String, Arc<CompiledClip>> = HashMap::new();
        let compiled = CompiledMontage::compile(asset, &rig.runtime, &mut |id| {
            if let Some(hit) = cache.get(id) {
                return Some(hit.clone());
            }
            let clip = Arc::new(CompiledClip::compile_shared(&source.sequence(id)?, &rig.runtime));
            cache.insert(id.to_string(), clip.clone());
            Some(clip)
        })
        .map_err(|m| err("preview.montage", m))?;
        let montage = Arc::new(compiled);
        instance.play_montage(montage.clone(), 1.0, None).map_err(|e| err("preview.play", format!("{e:?}")))?;
        let length = montage.length;
        Ok(Self { instance, montage, position: 0.0, length })
    }

    /// The pose at `time` seconds into the Montage. Moving forward advances the
    /// instance; moving back replays from the start, the way scrubbing a
    /// stateful runtime has to. Nothing is reported to gameplay either way.
    pub fn pose_at(&mut self, time: f32) -> Arc<Pose> {
        let time = time.clamp(0.0, self.length.max(0.0));
        if time < self.position {
            // a fresh instance on the same compiled graph; `shutdown` would drop the graph
            if let Some(graph) = self.instance.graph().cloned() {
                let mut fresh = AnimInstance::new(graph.skeleton.clone());
                if fresh.set_graph(graph).is_ok() {
                    let _ = fresh.play_montage(self.montage.clone(), 1.0, None);
                    self.instance = fresh;
                    self.position = 0.0;
                }
            }
        }
        let dt = time - self.position;
        self.position = time;
        self.instance.advance(dt, TimeStep::Variable, UpdateMode::Scrub)
    }

    pub fn instance(&self) -> &AnimInstance {
        &self.instance
    }
}

/// A Blend Space played at an x/y input.
pub struct BlendSpacePreview {
    instance: AnimInstance,
    pub input: (f32, f32),
}

impl BlendSpacePreview {
    pub fn build(asset: &BlendSpaceAsset, rig: &Rig, source: &dyn AssetSource) -> Result<Self, Vec<Diagnostic>> {
        let err = |m: String| vec![Diagnostic::error("preview.graph", m)];
        let mut graph = AnimationGraphAsset::default();
        graph.parameters.push(skeletal_assets::anim_graph::GraphParameter::new("X", ParamType::Float));
        graph.parameters.push(skeletal_assets::anim_graph::GraphParameter::new("Y", ParamType::Float));
        add(&mut graph, "x", "anim.get_parameter", 0.0, &[("parameter", json!("X"))]).map_err(err)?;
        add(&mut graph, "y", "anim.get_parameter", 0.0, &[("parameter", json!("Y"))]).map_err(err)?;
        add(&mut graph, "player", "anim.blend_space_player", 200.0, &[("blend_space", json!(asset.id.as_str())), ("looping", json!("loop"))]).map_err(err)?;
        wire(&mut graph, ("x", "value"), ("player", "x")).map_err(err)?;
        wire(&mut graph, ("y", "value"), ("player", "y")).map_err(err)?;
        wire(&mut graph, ("player", "pose"), ("output", "pose")).map_err(err)?;
        let instance = finish_with_skeleton(&graph, rig, source)?;
        Ok(Self { instance, input: (0.0, 0.0) })
    }

    pub fn set_input(&mut self, x: f32, y: f32) {
        self.input = (x, y);
        let _ = self.instance.set_float("X", x);
        let _ = self.instance.set_float("Y", y);
    }

    /// The pose the last advance produced.
    pub fn pose(&self) -> Arc<Pose> {
        self.instance.pose().clone()
    }

    /// Run the player forward by `dt` seconds and return the pose.
    pub fn advance(&mut self, dt: f32) -> Arc<Pose> {
        self.instance.advance(dt, TimeStep::Variable, UpdateMode::Scrub)
    }

    pub fn instance(&self) -> &AnimInstance {
        &self.instance
    }
}

fn finish_with_skeleton(graph: &AnimationGraphAsset, rig: &Rig, source: &dyn AssetSource) -> Result<AnimInstance, Vec<Diagnostic>> {
    // The preview graph names no skeleton of its own; the rig is the skeleton.
    let mut graph = graph.clone();
    graph.skeleton = Some(rig.skeleton_ref.clone());
    finish(&graph, source)
}

/// World matrices per bone id for `pose`, the form the viewport scene builder poses with.
pub fn world_of(rig: &Rig, pose: &Pose) -> HashMap<String, Mat4> {
    let mesh_space = rig.runtime.to_mesh_space(&pose.bones);
    let mut out = HashMap::with_capacity(mesh_space.len());
    for bone in &rig.skeleton.bones {
        if let Some(i) = rig.runtime.bone(&bone.id) {
            out.insert(bone.id.clone(), Mat4(mesh_space[i].to_cols_array()));
        }
    }
    out
}

/// Where a bone sits in the pose, for the tests and readouts.
pub fn bone_position(rig: &Rig, pose: &Pose, bone: &str) -> Option<Vec3> {
    world_of(rig, pose).get(bone).map(|m| m.transform_point(Vec3::ZERO))
}

/// Load the rig of skeleton `id` from the project, with the mesh the skeleton
/// names as its preview mesh or else the first mesh built for it.
pub fn load_rig(root: &Path, assets: &ProjectAssets, id: &str) -> Result<Rig, String> {
    use crate::anim_editors::refs::{ProjectRefIndex, RefIndex};
    use pulsar_reflection::{AssetKind, SkeletalMeshAssetKind};
    let skeleton = assets.skeleton(id).ok_or("The skeleton could not be loaded")?;
    let index = ProjectRefIndex::new(root);
    let preferred = skeleton.preview_mesh.as_ref().map(|m| m.as_str().to_string());
    let mesh = preferred
        .into_iter()
        .chain(index.candidates(SkeletalMeshAssetKind::KIND).into_iter().filter(|c| c.primary_skeleton.as_deref() == Some(id)).map(|c| c.id))
        .find_map(|mesh_id| {
            let path = index.locate_path(SkeletalMeshAssetKind::KIND, &mesh_id)?;
            skeletal_assets::skmesh::load_skmesh_asset(&path).ok()
        });
    Rig::new(&skeleton, mesh)
}

/// Offscreen skinned rendering of a posed rig.
pub struct SkinRenderer {
    gpu: crate::rendering::preview::Gpu,
    renderer: crate::rendering::viewport::renderer::ViewportRenderer,
}

pub struct SkinFrame {
    pub width: u32,
    pub height: u32,
    /// RGBA8, top row first.
    pub rgba: Vec<u8>,
}

impl SkinRenderer {
    /// `None` when the machine has no GPU adapter.
    pub fn new() -> Option<Self> {
        Some(Self { gpu: crate::rendering::preview::Gpu::new()?, renderer: crate::rendering::viewport::renderer::ViewportRenderer::new() })
    }

    pub fn render(&mut self, rig: &Rig, pose: &Pose, palette: &crate::style::Palette, width: u32, height: u32) -> SkinFrame {
        use crate::editor::panel::ViewOptions;
        use crate::rendering::viewport::camera::OrbitCamera;
        use crate::rendering::viewport::renderer::SceneFrame;
        use crate::rendering::viewport::scene::{build_gizmo, build_scene, SceneInput, SkinInput};
        use crate::rendering::viewport::types::ViewportUniforms;

        let world = world_of(rig, pose);
        let clip = crate::core::AnimationClip::bind_pose();
        let view = ViewOptions::default();
        let geo = build_scene(&SceneInput {
            skeleton: &rig.skeleton,
            sockets: None,
            clip: &clip,
            time: 0.0,
            range: (0.0, 1.0),
            selected: None,
            hovered: None,
            view,
            palette,
            tracers: None,
            skin: rig.mesh.as_ref().map(|mesh| SkinInput { mesh, weights_of: None }),
            physics: None,
            transform_gizmo: None,
            world_override: Some(&world),
        });
        let mut camera = OrbitCamera::default();
        camera.frame_sphere(geo.center, geo.radius);
        let aspect = width as f32 / height.max(1) as f32;
        let uniforms = ViewportUniforms::new(
            palette,
            camera.view_proj(aspect),
            camera.eye().to_array(),
            [width as f32, height as f32],
            (camera.distance * 8.0).max(30.0),
            geo.shadow(palette),
        );
        let gizmo = build_gizmo(&camera, width as f32, height as f32, palette);
        let renderer = &mut self.renderer;
        let rgba = self.gpu.render(width, height, |device, queue, target| {
            // a few frames let the temporal anti-aliasing settle on a still pose
            for _ in 0..4 {
                renderer.render_frame(
                    device,
                    queue,
                    target,
                    width,
                    height,
                    crate::rendering::preview::TARGET_FORMAT,
                    &SceneFrame {
                        uniforms: &uniforms,
                        mesh: &geo.mesh,
                        transform_gizmo: &geo.transform_gizmo_mesh,
                        skin: &geo.skin,
                        ghost: &geo.ghost,
                        ribbons: &geo.ribbons,
                        joints: &geo.joints,
                        gizmo_ribbons: &gizmo.ribbons,
                        gizmo_bubbles: &gizmo.bubbles,
                        show_grid: view.grid,
                        xray: view.xray,
                    },
                );
            }
        });
        SkinFrame { width, height, rgba }
    }
}

#[cfg(test)]
mod tests;
