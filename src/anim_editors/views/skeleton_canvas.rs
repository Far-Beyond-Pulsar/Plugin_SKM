//! A skeleton drawn flat on a canvas with chain, goal and selection overlays,
//! and pointer picking. Used by the IK Rig, Retarget and IK Retargeter editors;
//! the shape comes from [`SkeletonProjection`].

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;

use gpui::*;
use pulsar_reflection::{AssetKind, IkRigAssetKind, SkeletonAssetKind};
use skeletal_assets::assets::SkeletonAsset;
use skeletal_assets::ik_rig::IkRigAsset;
use skeletal_assets::skeleton::Skeleton;

use crate::anim_editors::refs::ProjectRefIndex;
use crate::anim_editors::skeleton_proj::SkeletonProjection;

use super::paint::{dot, pt, stroke_polyline};

pub const PAD: f32 = 24.0;

/// A run of bones drawn in one colour.
#[derive(Clone)]
pub struct Overlay {
    pub bones: Vec<String>,
    pub color: Hsla,
    pub width: f32,
}

/// A ring around a bone (an IK goal).
#[derive(Clone)]
pub struct Ring {
    pub bone: String,
    pub color: Hsla,
}

/// The skeleton asset `id` in the project, as a skeleton.
pub fn load_skeleton(root: &Path, id: &str) -> Option<Skeleton> {
    let path = ProjectRefIndex::new(root).locate_path(SkeletonAssetKind::KIND, id)?;
    let text = String::from_utf8(engine_fs::virtual_fs::read_file(&path).ok()?).ok()?;
    SkeletonAsset::parse(&text).ok().map(|a| a.skeleton())
}

/// The IK rig asset `id` in the project.
pub fn load_rig(root: &Path, id: &str) -> Option<IkRigAsset> {
    let path = ProjectRefIndex::new(root).locate_path(IkRigAssetKind::KIND, id)?;
    let text = String::from_utf8(engine_fs::virtual_fs::read_file(&path).ok()?).ok()?;
    IkRigAsset::parse(&text).ok()
}

/// Draw `projection` into a canvas that records where it was painted in `bounds`.
pub fn element(
    projection: SkeletonProjection,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    base: Hsla,
    background: Hsla,
    overlays: Vec<Overlay>,
    rings: Vec<Ring>,
    selected: Option<String>,
    accent: Hsla,
) -> AnyElement {
    canvas(
        move |b, _, _| {
            bounds.set(b);
            b
        },
        move |b, _, window, _| {
            if projection.is_empty() {
                return;
            }
            let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
            let (ox, oy) = (f32::from(b.origin.x), f32::from(b.origin.y));
            let at = projection.layout(w, h, PAD);
            let p = |i: usize| pt(ox + at[i].0, oy + at[i].1);
            for (i, bone) in projection.bones.iter().enumerate() {
                if let Some(parent) = bone.parent {
                    stroke_polyline(window, &[p(parent), p(i)], 1.5, base.opacity(0.7));
                }
            }
            for overlay in &overlays {
                let pts: Vec<_> = overlay.bones.iter().filter_map(|id| projection.index_of(id)).map(p).collect();
                stroke_polyline(window, &pts, overlay.width, overlay.color);
            }
            for i in 0..projection.bones.len() {
                dot(window, p(i), 3.0, base);
            }
            for ring in &rings {
                if let Some(i) = projection.index_of(&ring.bone) {
                    dot(window, p(i), 9.0, ring.color);
                    dot(window, p(i), 6.5, background);
                }
            }
            if let Some(i) = selected.as_deref().and_then(|s| projection.index_of(s)) {
                dot(window, p(i), 8.0, accent.opacity(0.4));
                dot(window, p(i), 4.5, accent);
            }
        },
    )
    .size_full()
    .into_any_element()
}

/// The bone under `position`, if any.
pub fn pick(projection: &SkeletonProjection, bounds: &Cell<Bounds<Pixels>>, position: Point<Pixels>, radius: f32) -> Option<String> {
    let b = bounds.get();
    projection.pick(f32::from(b.size.width), f32::from(b.size.height), PAD, f32::from(position.x - b.origin.x), f32::from(position.y - b.origin.y), radius)
}
