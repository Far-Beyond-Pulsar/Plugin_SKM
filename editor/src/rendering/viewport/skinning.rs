//! bind-pose packing and conservative bounds shared by the scene and gpu cache.

use std::{collections::HashMap, sync::Arc};

use crate::core::{
    skinning::joint_matrix,
    skmesh::{SkeletalMeshAsset, SkinnedVertex},
    Mat4, Vec3,
};

/// vertex attributes, not a storage struct: 64 bytes with no implicit padding.
/// joints are expanded from the file's u16 values to portable Uint32x4 attributes.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SkinVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub joints: [u32; 4],
    pub weights: [f32; 4],
}

impl From<SkinnedVertex> for SkinVertex {
    fn from(v: SkinnedVertex) -> Self {
        Self {
            position: v.position,
            normal: v.normal,
            uv: v.uv,
            joints: v.joints.map(u32::from),
            weights: v.weights,
        }
    }
}

/// column-major mat4 plus a vec4: 80-byte storage stride, aligned to 16 in wgsl.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SkinJoint {
    pub matrix: [f32; 16],
    pub selected: [f32; 4],
}

/// two vec4s (32 bytes); the second is interpreted as four u32s in wgsl.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct SkinUniforms {
    pub color: [f32; 4],
    pub params: [u32; 4],
}

#[derive(Clone, Copy, Debug)]
struct Bounds {
    min: Vec3,
    max: Vec3,
}

impl Bounds {
    fn include(bounds: &mut Option<Self>, p: Vec3) {
        match bounds {
            Some(b) => {
                b.min = Vec3::new(b.min.x.min(p.x), b.min.y.min(p.y), b.min.z.min(p.z));
                b.max = Vec3::new(b.max.x.max(p.x), b.max.y.max(p.y), b.max.z.max(p.z));
            }
            None => *bounds = Some(Self { min: p, max: p }),
        }
    }

    fn corners(self) -> [Vec3; 8] {
        std::array::from_fn(|i| {
            Vec3::new(
                if i & 1 == 0 { self.min.x } else { self.max.x },
                if i & 2 == 0 { self.min.y } else { self.max.y },
                if i & 4 == 0 { self.min.z } else { self.max.z },
            )
        })
    }
}

pub struct SkinLod {
    pub(crate) vertex_count: usize,
    pub(crate) indices: Vec<u32>,
    joint_bounds: Vec<Option<Bounds>>,
    unweighted: Option<Bounds>,
}

impl SkinLod {
    /// positive normalized blends lie inside the union of the joint boxes.
    /// transform eight corners per joint instead of skinning every vertex.
    pub fn posed_bounds(&self, joints: &[SkinJoint]) -> Option<[Vec3; 8]> {
        let mut bounds = self.unweighted;
        for (b, joint) in self.joint_bounds.iter().zip(joints) {
            if let Some(b) = b {
                for p in b.corners() {
                    Bounds::include(&mut bounds, Mat4(joint.matrix).transform_point(p));
                }
            }
        }
        bounds.map(|mut b| {
            // leave room for rounding in the weighted gpu arithmetic.
            let extent = b.min.length().max(b.max.length()).max(1.0) * 2e-6;
            let pad = Vec3::new(extent, extent, extent);
            b.min = b.min.sub(pad);
            b.max = b.max.add(pad);
            b.corners()
        })
    }
}

pub struct PreparedSkin {
    pub(crate) asset: Arc<SkeletalMeshAsset>,
    pub(crate) lods: Vec<SkinLod>,
}

impl PreparedSkin {
    // only pack a lod when it is first uploaded; don't retain a second cpu vertex array.
    pub(crate) fn pack_vertices(&self, lod: usize) -> Vec<SkinVertex> {
        self.asset.lods[lod]
            .vertices
            .iter()
            .copied()
            .map(SkinVertex::from)
            .collect()
    }

    pub fn new(asset: Arc<SkeletalMeshAsset>) -> Result<Arc<Self>, &'static str> {
        let mut lods = Vec::with_capacity(asset.lods.len());
        for lod in &asset.lods {
            let mut joint_bounds = vec![None; asset.joints.len()];
            let mut unweighted = None;
            for v in &lod.vertices {
                if v.position
                    .iter()
                    .chain(&v.normal)
                    .chain(&v.uv)
                    .chain(&v.weights)
                    .any(|f| !f.is_finite())
                {
                    return Err("skeletal preview contains non-finite vertex data");
                }
                let p = Vec3::new(v.position[0], v.position[1], v.position[2]);
                let mut usable = false;
                for (&j, &w) in v.joints.iter().zip(&v.weights) {
                    if w > 0.0 {
                        if let Some(b) = joint_bounds.get_mut(j as usize) {
                            Bounds::include(b, p);
                            usable = true;
                        }
                    }
                }
                if !usable {
                    Bounds::include(&mut unweighted, p);
                }
            }
            // keep the old preview's handling of damaged triangles and trailing indices.
            let indices = lod
                .indices
                .as_chunks::<3>()
                .0
                .iter()
                .filter(|tri| tri.iter().all(|&i| (i as usize) < lod.vertices.len()))
                .flatten()
                .copied()
                .collect();
            lods.push(SkinLod {
                vertex_count: lod.vertices.len(),
                indices,
                joint_bounds,
                unweighted,
            });
        }
        Ok(Arc::new(Self { asset, lods }))
    }
}

/// one pose snapshot, shared with the skeleton overlay built from the same world map.
pub struct SkinFrame {
    pub(crate) mesh: Arc<PreparedSkin>,
    pub(crate) lod: usize,
    pub(crate) joints: Vec<SkinJoint>,
    pub(crate) uniforms: SkinUniforms,
}

impl SkinFrame {
    pub fn new(
        mesh: Arc<PreparedSkin>,
        lod: usize,
        world: &HashMap<String, Mat4>,
        selected: Option<&str>,
        color: [f32; 4],
    ) -> Option<Self> {
        let lod = lod.min(mesh.lods.len().saturating_sub(1));
        mesh.lods.get(lod)?;
        let joints: Vec<_> = mesh
            .asset
            .joints
            .iter()
            .map(|joint| SkinJoint {
                matrix: joint_matrix(joint, world).0,
                selected: [
                    if selected == Some(joint.bone.as_str()) {
                        1.0
                    } else {
                        0.0
                    },
                    0.0,
                    0.0,
                    0.0,
                ],
            })
            .collect();
        if joints
            .iter()
            .any(|j| j.matrix.iter().any(|x| !x.is_finite()))
        {
            return None;
        }
        let count = u32::try_from(mesh.asset.joints.len()).ok()?;
        Some(Self {
            mesh,
            lod,
            joints,
            uniforms: SkinUniforms {
                color,
                params: [count, u32::from(selected.is_some()), 0, 0],
            },
        })
    }
}

/// retain the old arc so in-place edits use copy-on-write and cannot fool this cache.
#[derive(Default)]
pub(crate) struct SkinCache {
    source: Option<Arc<SkeletalMeshAsset>>,
    revision: u64,
    prepared: Option<Arc<PreparedSkin>>,
}

impl SkinCache {
    pub(crate) fn get(
        &mut self,
        asset: Option<&Arc<SkeletalMeshAsset>>,
        revision: u64,
    ) -> Option<Arc<PreparedSkin>> {
        let same = match (&self.source, asset) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if !same || self.revision != revision {
            self.source = asset.cloned();
            self.revision = revision;
            self.prepared = asset.and_then(|a| match PreparedSkin::new(a.clone()) {
                Ok(mesh) => Some(mesh),
                Err(error) => {
                    log::warn!("{error}");
                    None
                }
            });
        }
        self.prepared.clone()
    }
}

#[cfg(test)]
mod benchmark;
#[cfg(test)]
mod gpu_tests;
#[cfg(test)]
pub(crate) mod tests;
