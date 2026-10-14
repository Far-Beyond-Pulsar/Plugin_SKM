//! The skeletal mesh asset (`.skmesh`): skinned geometry for one skeleton.
//!
//! # File format
//!
//! Skinned meshes are large and mostly numbers, so unlike the JSON skeleton and
//! sequence files this one is binary: a small JSON header for everything a
//! person or the project index wants to read, then the raw geometry. This is
//! the layout of the engine's `.mesh` files (a header, then packed buffers),
//! with a JSON header in place of fixed fields because the joint table and
//! material slots are variable length. All numbers are little-endian.
//!
//! ```text
//! offset  size  field
//! 0       4     magic "PSKM"
//! 4       4     format version (u32), currently 1
//! 8       8     header length in bytes (u64)
//! 16      H     header: UTF-8 JSON, see below
//! 16+H    ..    payload: for each LOD in header order, its vertices then its indices
//! ```
//!
//! The JSON header holds the mesh's own [`SkeletalMeshId`], the [`SkeletonId`]
//! it is skinned to, the joint table (bone id and inverse bind matrix per
//! joint), the material slots and, per LOD, the vertex and index counts and
//! the sections. A vertex is 56 bytes: position (3 x f32), normal (3 x f32),
//! uv (2 x f32), joint indices (4 x u16) and weights (4 x f32). An index is a
//! u32. A vertex has at most 4 influences; an unused one has weight 0.
//!
//! Joint indices are positions in the header's joint table, not bone indices.
//! The table maps each joint to a bone *id* of the referenced skeleton, so the
//! mesh survives bones being reordered in the skeleton.
//!
//! # Limitations
//!
//! Material slots carry a typed stable material id and retain a project-relative
//! path cache for older readers. There is one LOD today, but the layout is a
//! list so more can be added without a format change.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::ids::{SkeletalMeshId, SkeletonId};
use crate::issue::{Issue, Severity};
use crate::math::Mat4;
use crate::skeleton::Skeleton;

/// File extension of skeletal mesh assets.
pub const SKMESH_EXTENSION: &str = "skmesh";

const MAGIC: &[u8; 4] = b"PSKM";
/// Newest format version this build reads and writes. Newer files are refused.
pub const SKMESH_VERSION: u32 = 1;
const PREFIX: usize = 4 + 4 + 8;
const MAX_HEADER_BYTES: usize = 16 * 1024 * 1024;
/// Bytes per vertex in the payload.
const VERTEX_BYTES: usize = (3 + 3 + 2) * 4 + 4 * 2 + 4 * 4;

/// Weights may differ from a sum of 1 by this much (they are stored as f32).
pub const WEIGHT_TOLERANCE: f32 = 1e-3;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SkinnedVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    /// Positions in [`SkeletalMeshAsset::joints`].
    pub joints: [u16; 4],
    /// Influence of each joint; sums to 1 for a skinned vertex.
    pub weights: [f32; 4],
}

impl SkinnedVertex {
    /// Number of influences with a weight above zero.
    pub fn influences(&self) -> usize {
        self.weights.iter().filter(|w| **w > 0.0).count()
    }
}

/// A run of triangles drawn with one material slot.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshSection {
    pub first_index: u32,
    pub index_count: u32,
    pub material_slot: u32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshLod {
    pub vertices: Vec<SkinnedVertex>,
    /// Triangle list into `vertices`.
    pub indices: Vec<u32>,
    pub sections: Vec<MeshSection>,
}

/// One entry of the joint table.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Joint {
    /// Id of the bone in the referenced skeleton this joint follows.
    pub bone: String,
    /// Column-major matrix taking a mesh-space point into the bone's space at
    /// bind pose.
    pub inverse_bind: [f32; 16],
}

impl Joint {
    pub fn inverse_bind_matrix(&self) -> Mat4 {
        Mat4(self.inverse_bind)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterialSlot {
    pub name: String,
    /// Stable material identity. The path below is retained as a cache and as
    /// a migration source for files written before material ids were added.
    #[serde(default)]
    pub material_id: Option<pulsar_reflection::MaterialRef>,
    /// Legacy project-relative path of the material, if one is assigned.
    /// New references use `material_id`; this is kept so older readers and
    /// render paths can continue to resolve the material.
    #[serde(default)]
    pub material: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SkeletalMeshAsset {
    pub id: SkeletalMeshId,
    /// The skeleton this mesh is skinned to. `None` until one is picked; kept
    /// as is even when no skeleton with this id exists in the project.
    pub skeleton: Option<SkeletonId>,
    pub joints: Vec<Joint>,
    pub material_slots: Vec<MaterialSlot>,
    pub lods: Vec<MeshLod>,
}

#[derive(Serialize, Deserialize)]
struct Header {
    id: SkeletalMeshId,
    #[serde(default)]
    skeleton: Option<SkeletonId>,
    #[serde(default)]
    joints: Vec<Joint>,
    #[serde(default)]
    material_slots: Vec<MaterialSlot>,
    #[serde(default)]
    lods: Vec<LodHeader>,
}

#[derive(Serialize, Deserialize)]
struct LodHeader {
    vertex_count: u32,
    index_count: u32,
    #[serde(default)]
    sections: Vec<MeshSection>,
}

impl SkeletalMeshAsset {
    /// A mesh with no geometry yet and a fresh id.
    pub fn new(skeleton: Option<SkeletonId>) -> Self {
        Self {
            id: SkeletalMeshId::new(),
            skeleton,
            joints: Vec::new(),
            material_slots: Vec::new(),
            lods: Vec::new(),
        }
    }

    /// The most detailed LOD, if the mesh has any geometry.
    pub fn lod0(&self) -> Option<&MeshLod> {
        self.lods.first()
    }

    /// The file's bytes.
    pub fn encode(&self) -> Vec<u8> {
        let header = Header {
            id: self.id.clone(),
            skeleton: self.skeleton.clone(),
            joints: self.joints.clone(),
            material_slots: self.material_slots.clone(),
            lods: self
                .lods
                .iter()
                .map(|lod| LodHeader {
                    vertex_count: lod.vertices.len() as u32,
                    index_count: lod.indices.len() as u32,
                    sections: lod.sections.clone(),
                })
                .collect(),
        };
        // Serializing plain data cannot fail.
        let mut header = serde_json::to_vec(&header).unwrap_or_default();
        // Pad (JSON ignores trailing spaces) so the payload starts 4-byte aligned.
        while header.len() % 4 != 0 {
            header.push(b' ');
        }

        let payload: usize = self
            .lods
            .iter()
            .map(|l| l.vertices.len() * VERTEX_BYTES + l.indices.len() * 4)
            .sum();
        let mut out = Vec::with_capacity(PREFIX + header.len() + payload);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&SKMESH_VERSION.to_le_bytes());
        out.extend_from_slice(&(header.len() as u64).to_le_bytes());
        out.extend_from_slice(&header);
        for lod in &self.lods {
            for v in &lod.vertices {
                for f in v.position.iter().chain(&v.normal).chain(&v.uv) {
                    out.extend_from_slice(&f.to_le_bytes());
                }
                for j in v.joints {
                    out.extend_from_slice(&j.to_le_bytes());
                }
                for w in v.weights {
                    out.extend_from_slice(&w.to_le_bytes());
                }
            }
            for i in &lod.indices {
                out.extend_from_slice(&i.to_le_bytes());
            }
        }
        out
    }

    /// Parse a file. A truncated or oversized payload, or a version newer
    /// than [`SKMESH_VERSION`], is an error rather than a half-read mesh.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let (header, mut at) = read_header(bytes)?;
        let mut lods = Vec::with_capacity(header.lods.len());
        for lod in &header.lods {
            let (vc, ic) = (lod.vertex_count as usize, lod.index_count as usize);
            let end = vc
                .checked_mul(VERTEX_BYTES)
                .and_then(|v| ic.checked_mul(4).and_then(|i| v.checked_add(i)))
                .and_then(|n| at.checked_add(n))
                .filter(|end| *end <= bytes.len())
                .ok_or("the geometry is cut short")?;
            let mut r = Reader { bytes, at };
            let vertices = (0..vc).map(|_| r.vertex()).collect();
            let indices = (0..ic).map(|_| r.u32()).collect();
            at = end;
            lods.push(MeshLod {
                vertices,
                indices,
                sections: lod.sections.clone(),
            });
        }
        if at != bytes.len() {
            return Err(format!(
                "{} unexpected bytes after the geometry",
                bytes.len() - at
            ));
        }
        Ok(Self {
            id: header.id,
            skeleton: header.skeleton,
            joints: header.joints,
            material_slots: header.material_slots,
            lods,
        })
    }

    /// The id in a file's bytes, reading only the header. Works on files whose
    /// geometry is damaged, so the project index can still list them.
    pub fn peek_id(bytes: &[u8]) -> Option<SkeletalMeshId> {
        read_header(bytes).ok().map(|(h, _)| h.id)
    }

    /// Read only the fixed prefix and JSON header from a `.skmesh` on disk.
    /// The geometry payload is never loaded, even for large meshes.
    pub fn peek_id_at(path: &Path) -> anyhow::Result<SkeletalMeshId> {
        Self::peek_header_at(path).map(|(id, _)| id)
    }

    /// Read the identity and skeleton reference from a `.skmesh` header only.
    pub fn peek_header_at(path: &Path) -> anyhow::Result<(SkeletalMeshId, Option<SkeletonId>)> {
        let prefix = engine_fs::virtual_fs::read_file_prefix(path, PREFIX)?;
        if prefix.len() < PREFIX || &prefix[..4] != MAGIC {
            anyhow::bail!("not a skeletal mesh file");
        }
        let header_len = usize::try_from(u64::from_le_bytes(prefix[8..16].try_into()?))?;
        if header_len > MAX_HEADER_BYTES {
            anyhow::bail!("skeletal mesh header is too large");
        }
        let bytes = engine_fs::virtual_fs::read_file_prefix(path, PREFIX + header_len)?;
        let (header, _) = read_header(&bytes).map_err(anyhow::Error::msg)?;
        Ok((header.id, header.skeleton))
    }

    /// The skeleton a file's bytes refer to, reading only the header.
    pub fn peek_skeleton(bytes: &[u8]) -> Option<SkeletonId> {
        read_header(bytes).ok().and_then(|(h, _)| h.skeleton)
    }

    /// Counts for the mesh info panel.
    pub fn stats(&self) -> MeshStats {
        let lod = self.lod0();
        MeshStats {
            vertices: lod.map_or(0, |l| l.vertices.len()),
            triangles: lod.map_or(0, |l| l.indices.len() / 3),
            joints: self.joints.len(),
            max_influences: lod
                .and_then(|l| l.vertices.iter().map(SkinnedVertex::influences).max())
                .unwrap_or(0),
            lods: self.lods.len(),
            sections: lod.map_or(0, |l| l.sections.len()),
            material_slots: self.material_slots.len(),
        }
    }
}

/// What the mesh info panel shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MeshStats {
    pub vertices: usize,
    pub triangles: usize,
    pub joints: usize,
    /// The most joints any vertex of LOD 0 is influenced by (at most 4).
    pub max_influences: usize,
    pub lods: usize,
    pub sections: usize,
    pub material_slots: usize,
}

fn read_header(bytes: &[u8]) -> Result<(Header, usize), String> {
    if bytes.len() < PREFIX || &bytes[0..4] != MAGIC {
        return Err("not a skeletal mesh file".to_string());
    }
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap_or_default());
    if version > SKMESH_VERSION {
        return Err(format!(
            "file format version {version} is newer than this editor supports ({SKMESH_VERSION})"
        ));
    }
    let len = u64::from_le_bytes(bytes[8..16].try_into().unwrap_or_default());
    let end = usize::try_from(len)
        .ok()
        .and_then(|len| PREFIX.checked_add(len))
        .filter(|end| *end <= bytes.len())
        .ok_or("the header is cut short")?;
    let header =
        serde_json::from_slice(&bytes[PREFIX..end]).map_err(|e| format!("bad header: {e}"))?;
    Ok((header, end))
}

/// Sequential little-endian reads; callers have checked the length.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let out = self.bytes[self.at..self.at + N]
            .try_into()
            .unwrap_or([0; N]);
        self.at += N;
        out
    }
    fn f32(&mut self) -> f32 {
        f32::from_le_bytes(self.take())
    }
    fn u16(&mut self) -> u16 {
        u16::from_le_bytes(self.take())
    }
    fn u32(&mut self) -> u32 {
        u32::from_le_bytes(self.take())
    }
    fn vertex(&mut self) -> SkinnedVertex {
        SkinnedVertex {
            position: [self.f32(), self.f32(), self.f32()],
            normal: [self.f32(), self.f32(), self.f32()],
            uv: [self.f32(), self.f32()],
            joints: [self.u16(), self.u16(), self.u16(), self.u16()],
            weights: [self.f32(), self.f32(), self.f32(), self.f32()],
        }
    }
}

pub fn load_skmesh_asset(path: &Path) -> anyhow::Result<SkeletalMeshAsset> {
    let bytes = engine_fs::virtual_fs::read_file(path)?;
    SkeletalMeshAsset::decode(&bytes).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}

pub fn save_skmesh_asset(path: &Path, asset: &SkeletalMeshAsset) -> anyhow::Result<()> {
    engine_fs::virtual_fs::write_file_atomically(path, &asset.encode())
}

/// Everything wrong with `mesh`, given the skeleton its reference resolved to
/// (`None` when it did not resolve; the bone checks are then skipped, and the
/// editor reports the unresolved link separately).
///
/// Only reports: no vertex, joint or bone is ever changed or removed.
pub fn validate_mesh(mesh: &SkeletalMeshAsset, skeleton: Option<&Skeleton>) -> Vec<Issue> {
    let mut issues = Vec::new();
    let mut error = |message: String| issues.push(Issue::new(Severity::Error, message));

    if mesh.lods.is_empty() {
        error("The mesh has no geometry.".to_string());
    }
    for (n, joint) in mesh.joints.iter().enumerate() {
        if joint.inverse_bind.iter().any(|v| !v.is_finite()) {
            error(format!(
                "Joint {n} (\"{}\") has an inverse bind matrix with invalid numbers.",
                joint.bone
            ));
        }
    }

    for (lod_index, lod) in mesh.lods.iter().enumerate() {
        let lod_label = if mesh.lods.len() > 1 {
            format!("LOD {lod_index}: ")
        } else {
            String::new()
        };

        // Weights and joint indices, collected per kind so one bad mesh gives
        // a few lines, not one per vertex.
        let (mut unnormalized, mut unweighted, mut bad_joint) =
            (Vec::new(), Vec::new(), Vec::new());
        for (v, vertex) in lod.vertices.iter().enumerate() {
            let sum: f32 = vertex.weights.iter().sum();
            if vertex.weights.iter().any(|w| !w.is_finite() || *w < 0.0) {
                unnormalized.push(v);
            } else if sum <= 0.0 {
                unweighted.push(v);
            } else if (sum - 1.0).abs() > WEIGHT_TOLERANCE {
                unnormalized.push(v);
            }
            let out_of_range = vertex
                .joints
                .iter()
                .zip(&vertex.weights)
                .any(|(j, w)| *w > 0.0 && *j as usize >= mesh.joints.len());
            if out_of_range {
                bad_joint.push(v);
            }
        }
        for (list, what) in [
            (
                &unnormalized,
                "have weights that are negative, invalid or do not add up to 1",
            ),
            (&unweighted, "have no weights"),
            (
                &bad_joint,
                "use a joint index that is not in the joint table",
            ),
        ] {
            if !list.is_empty() {
                error(format!(
                    "{lod_label}{} vertices {what} ({}).",
                    list.len(),
                    examples(list)
                ));
            }
        }

        // Topology.
        if lod.indices.len() % 3 != 0 {
            error(format!(
                "{lod_label}The index count ({}) is not a multiple of 3.",
                lod.indices.len()
            ));
        }
        if let Some(bad) = lod
            .indices
            .iter()
            .find(|i| **i as usize >= lod.vertices.len())
        {
            error(format!(
                "{lod_label}An index ({bad}) points past the last vertex ({}).",
                lod.vertices.len()
            ));
        }
        for (s, section) in lod.sections.iter().enumerate() {
            let end = section.first_index as usize + section.index_count as usize;
            if end > lod.indices.len() {
                error(format!(
                    "{lod_label}Section {s} reaches past the end of the index buffer."
                ));
            }
            if section.material_slot as usize >= mesh.material_slots.len() {
                error(format!(
                    "{lod_label}Section {s} uses material slot {}, but there are {} slots.",
                    section.material_slot,
                    mesh.material_slots.len()
                ));
            }
        }
    }

    if let Some(skeleton) = skeleton {
        let missing: Vec<&str> = mesh
            .joints
            .iter()
            .map(|j| j.bone.as_str())
            .filter(|bone| skeleton.bone(bone).is_none())
            .collect();
        if !missing.is_empty() {
            let mut names: Vec<&str> = missing.iter().take(3).copied().collect();
            if missing.len() > names.len() {
                names.push("...");
            }
            error(format!(
                "{} joint(s) follow bones that are not in the skeleton: {}.",
                missing.len(),
                names.join(", ")
            ));
        }
    }

    // Two joints following one bone is legal but usually a mistake.
    let mut seen = std::collections::HashSet::new();
    let dupes: Vec<&str> = mesh
        .joints
        .iter()
        .map(|j| j.bone.as_str())
        .filter(|b| !seen.insert(*b))
        .collect();
    if !dupes.is_empty() {
        issues.push(Issue::new(
            Severity::Warning,
            format!(
                "More than one joint follows the same bone: {}.",
                dupes.join(", ")
            ),
        ));
    }
    issues
}

/// The first few vertex numbers of `list`, for a message.
fn examples(list: &[usize]) -> String {
    let mut shown: Vec<String> = list.iter().take(3).map(|v| v.to_string()).collect();
    if list.len() > shown.len() {
        shown.push("...".to_string());
    }
    format!("vertex {}", shown.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::Transform;
    use crate::skeleton::Bone;

    fn skeleton() -> Skeleton {
        let bone = |id: &str, parent: Option<&str>| Bone {
            id: id.into(),
            name: id.into(),
            parent: parent.map(str::to_string),
            bind_transform: Transform::IDENTITY,
        };
        Skeleton {
            bones: vec![bone("root", None), bone("arm", Some("root"))],
        }
    }

    /// A one-triangle mesh skinned to `root` and `arm`.
    pub(crate) fn triangle() -> SkeletalMeshAsset {
        let vertex = |x: f32, joints: [u16; 4], weights: [f32; 4]| SkinnedVertex {
            position: [x, 0.0, 0.0],
            normal: [0.0, 0.0, 1.0],
            uv: [x, 0.5],
            joints,
            weights,
        };
        let identity = Mat4::IDENTITY.0;
        SkeletalMeshAsset {
            id: SkeletalMeshId::new(),
            skeleton: Some(SkeletonId::new()),
            joints: vec![
                Joint {
                    bone: "root".into(),
                    inverse_bind: identity,
                },
                Joint {
                    bone: "arm".into(),
                    inverse_bind: identity,
                },
            ],
            material_slots: vec![MaterialSlot {
                name: "Body".into(),
                material_id: None,
                material: Some("mats/Body.mat".into()),
            }],
            lods: vec![MeshLod {
                vertices: vec![
                    vertex(0.0, [0, 0, 0, 0], [1.0, 0.0, 0.0, 0.0]),
                    vertex(1.0, [0, 1, 0, 0], [0.25, 0.75, 0.0, 0.0]),
                    vertex(2.0, [1, 0, 0, 0], [1.0, 0.0, 0.0, 0.0]),
                ],
                indices: vec![0, 1, 2],
                sections: vec![MeshSection {
                    first_index: 0,
                    index_count: 3,
                    material_slot: 0,
                }],
            }],
        }
    }

    fn messages(issues: &[Issue]) -> String {
        issues
            .iter()
            .map(|i| i.message.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_mesh_round_trips_through_its_bytes() {
        let mesh = triangle();
        let bytes = mesh.encode();
        assert_eq!(&bytes[0..4], b"PSKM");
        assert_eq!(SkeletalMeshAsset::decode(&bytes).unwrap(), mesh);
        assert_eq!(SkeletalMeshAsset::peek_id(&bytes), Some(mesh.id.clone()));
        assert_eq!(SkeletalMeshAsset::peek_skeleton(&bytes), mesh.skeleton);
        assert_eq!(bytes.len() % 4, 0);
    }

    #[test]
    fn an_empty_mesh_and_several_lods_round_trip() {
        let empty = SkeletalMeshAsset::new(None);
        assert_eq!(SkeletalMeshAsset::decode(&empty.encode()).unwrap(), empty);

        let mut mesh = triangle();
        let lod = mesh.lods[0].clone();
        mesh.lods.push(MeshLod {
            indices: vec![2, 1, 0],
            ..lod
        });
        assert_eq!(SkeletalMeshAsset::decode(&mesh.encode()).unwrap(), mesh);
    }

    #[test]
    fn damaged_files_are_refused_not_half_read() {
        let bytes = triangle().encode();
        assert!(SkeletalMeshAsset::decode(&bytes[..bytes.len() - 1]).is_err());
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(SkeletalMeshAsset::decode(&longer).is_err());
        assert!(SkeletalMeshAsset::decode(b"nope").is_err());
        assert!(SkeletalMeshAsset::decode(&bytes[..20]).is_err());

        let mut newer = bytes.clone();
        newer[4..8].copy_from_slice(&(SKMESH_VERSION + 1).to_le_bytes());
        assert!(SkeletalMeshAsset::decode(&newer)
            .unwrap_err()
            .contains("newer"));

        // The id is still readable from a file whose geometry is cut off.
        let cut = &bytes[..bytes.len() - 8];
        assert!(SkeletalMeshAsset::peek_id(cut).is_some());
    }

    #[test]
    fn files_round_trip_through_the_virtual_fs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Hero.skmesh");
        let mesh = triangle();
        save_skmesh_asset(&path, &mesh).unwrap();
        assert_eq!(load_skmesh_asset(&path).unwrap(), mesh);
    }

    #[test]
    fn stats_count_the_first_lod() {
        let stats = triangle().stats();
        assert_eq!(
            stats,
            MeshStats {
                vertices: 3,
                triangles: 1,
                joints: 2,
                max_influences: 2,
                lods: 1,
                sections: 1,
                material_slots: 1
            }
        );
        assert_eq!(SkeletalMeshAsset::new(None).stats(), MeshStats::default());
    }

    #[test]
    fn a_clean_mesh_has_no_issues() {
        assert!(validate_mesh(&triangle(), Some(&skeleton())).is_empty());
        assert!(validate_mesh(&triangle(), None).is_empty());
    }

    #[test]
    fn bad_weights_are_reported() {
        let mut mesh = triangle();
        mesh.lods[0].vertices[1].weights = [0.5, 0.75, 0.0, 0.0];
        mesh.lods[0].vertices[2].weights = [0.0; 4];
        let issues = validate_mesh(&mesh, Some(&skeleton()));
        let text = messages(&issues);
        assert!(
            text.contains("1 vertices have weights") && text.contains("vertex 1"),
            "{text}"
        );
        assert!(
            text.contains("1 vertices have no weights") && text.contains("vertex 2"),
            "{text}"
        );
        assert!(issues.iter().all(|i| i.severity == Severity::Error));

        mesh.lods[0].vertices[1].weights = [-0.5, 1.5, 0.0, 0.0];
        assert!(messages(&validate_mesh(&mesh, None)).contains("negative"));
    }

    #[test]
    fn a_joint_index_outside_the_table_is_reported_but_not_when_unweighted() {
        let mut mesh = triangle();
        mesh.lods[0].vertices[0].joints = [9, 0, 0, 0];
        assert!(messages(&validate_mesh(&mesh, None)).contains("joint index"));

        // An unused influence slot may hold any index.
        mesh = triangle();
        mesh.lods[0].vertices[0].joints = [0, 9, 0, 0];
        assert!(validate_mesh(&mesh, None).is_empty());
    }

    #[test]
    fn joints_for_bones_missing_from_the_skeleton_are_reported_and_kept() {
        let mut mesh = triangle();
        mesh.joints[1].bone = "tail".into();
        let issues = validate_mesh(&mesh, Some(&skeleton()));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, Severity::Error);
        assert!(issues[0].message.contains("tail"));
        assert_eq!(mesh.joints.len(), 2, "nothing is deleted");
        // Without a skeleton there is nothing to compare against.
        assert!(validate_mesh(&mesh, None).is_empty());
    }

    #[test]
    fn topology_and_slot_problems_are_reported() {
        let mut mesh = triangle();
        mesh.lods[0].indices = vec![0, 1, 9, 0];
        mesh.lods[0].sections[0].material_slot = 4;
        mesh.lods[0].sections[0].index_count = 6;
        let text = messages(&validate_mesh(&mesh, None));
        assert!(text.contains("multiple of 3"), "{text}");
        assert!(text.contains("past the last vertex"), "{text}");
        assert!(text.contains("material slot 4"), "{text}");
        assert!(text.contains("reaches past"), "{text}");
        assert!(
            messages(&validate_mesh(&SkeletalMeshAsset::new(None), None)).contains("no geometry")
        );
    }

    #[test]
    fn two_joints_on_one_bone_warn() {
        let mut mesh = triangle();
        mesh.joints[1].bone = "root".into();
        let issues = validate_mesh(&mesh, Some(&skeleton()));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, Severity::Warning);
    }
}
