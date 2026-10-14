//! Higher-level animation assets built from stable, typed asset references.
//!
//! These formats intentionally store GUID references instead of paths, so a
//! sequence, skeleton, blend space, or graph remains linked when moved.

use std::path::Path;

use pulsar_reflection::{
    AnimationGraphAssetKind, AnimationMontageAssetKind, AnimationSequenceAssetKind, AssetRef,
    BlendSpaceAssetKind, SkeletonAssetKind,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

pub const BLEND_SPACE_EXTENSION: &str = "blendspace";
pub const MONTAGE_EXTENSION: &str = "montage";
pub const ANIMATION_GRAPH_EXTENSION: &str = "animgraph";
pub const FORMAT_VERSION: u32 = 1;

fn current_version() -> u32 {
    FORMAT_VERSION
}

/// An axis used to place animation clips in a 1D or 2D blend space.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BlendAxis {
    pub name: String,
    pub min: f32,
    pub max: f32,
}

impl Default for BlendAxis {
    fn default() -> Self {
        Self {
            name: "Speed".into(),
            min: 0.0,
            max: 1.0,
        }
    }
}

/// One clip placed at a parameter coordinate in a blend space.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlendSample {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<AssetRef<AnimationSequenceAssetKind>>,
    pub x: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f32>,
}

/// A parameterized collection of animation sequences for runtime blending.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlendSpaceAsset {
    #[serde(default = "current_version")]
    pub version: u32,
    #[serde(default)]
    pub id: AssetRef<BlendSpaceAssetKind>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skeleton: Option<AssetRef<SkeletonAssetKind>>,
    pub x_axis: BlendAxis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y_axis: Option<BlendAxis>,
    #[serde(default)]
    pub samples: Vec<BlendSample>,
}

impl Default for BlendSpaceAsset {
    fn default() -> Self {
        Self {
            version: FORMAT_VERSION,
            id: AssetRef::new(),
            name: "New Blend Space".into(),
            skeleton: None,
            x_axis: BlendAxis::default(),
            y_axis: None,
            samples: Vec::new(),
        }
    }
}

/// A section in a montage timeline. Times are expressed in seconds.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MontageSection {
    pub name: String,
    pub start: f32,
    pub end: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_section: Option<String>,
}

/// One sequence playback segment in a montage.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MontageSegment {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<AssetRef<AnimationSequenceAssetKind>>,
    pub start: f32,
    #[serde(default = "one")]
    pub play_rate: f32,
    #[serde(default)]
    pub looping: bool,
}

fn one() -> f32 {
    1.0
}

/// Ordered sequences with named sections and optional playback scaling.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnimationMontageAsset {
    #[serde(default = "current_version")]
    pub version: u32,
    #[serde(default)]
    pub id: AssetRef<AnimationMontageAssetKind>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skeleton: Option<AssetRef<SkeletonAssetKind>>,
    #[serde(default)]
    pub segments: Vec<MontageSegment>,
    #[serde(default)]
    pub sections: Vec<MontageSection>,
}

impl Default for AnimationMontageAsset {
    fn default() -> Self {
        Self {
            version: FORMAT_VERSION,
            id: AssetRef::new(),
            name: "New Montage".into(),
            skeleton: None,
            segments: Vec::new(),
            sections: Vec::new(),
        }
    }
}

/// A typed node in an animation graph. Output is the terminal pose node.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnimationNode {
    Sequence {
        sequence: Option<AssetRef<AnimationSequenceAssetKind>>,
    },
    BlendSpace {
        blend_space: Option<AssetRef<BlendSpaceAssetKind>>,
    },
    Output,
}

/// A directed connection between graph nodes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnimationConnection {
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnimationGraphNode {
    pub id: String,
    pub name: String,
    #[serde(flatten)]
    pub node: AnimationNode,
}

/// A serializable animation graph definition tied to one skeleton.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnimationGraphAsset {
    #[serde(default = "current_version")]
    pub version: u32,
    #[serde(default)]
    pub id: AssetRef<AnimationGraphAssetKind>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skeleton: Option<AssetRef<SkeletonAssetKind>>,
    #[serde(default)]
    pub nodes: Vec<AnimationGraphNode>,
    #[serde(default)]
    pub connections: Vec<AnimationConnection>,
}

impl Default for AnimationGraphAsset {
    fn default() -> Self {
        Self {
            version: FORMAT_VERSION,
            id: AssetRef::new(),
            name: "New Animation Graph".into(),
            skeleton: None,
            nodes: vec![AnimationGraphNode {
                id: "output".into(),
                name: "Output".into(),
                node: AnimationNode::Output,
            }],
            connections: Vec::new(),
        }
    }
}

fn parse_versioned<T: DeserializeOwned>(text: &str) -> Result<T, String> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if let Some(version) = value.get("version").and_then(|v| v.as_u64()) {
        if version > FORMAT_VERSION as u64 {
            return Err(format!("file format version {version} is newer than this editor supports ({FORMAT_VERSION})"));
        }
    }
    serde_json::from_value(value).map_err(|e| e.to_string())
}

macro_rules! asset_io {
    ($asset:ty, $load:ident, $save:ident) => {
        pub fn $load(path: &Path) -> anyhow::Result<$asset> {
            let text = String::from_utf8(engine_fs::virtual_fs::read_file(path)?)?;
            parse_versioned(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
        }
        pub fn $save(path: &Path, asset: &$asset) -> anyhow::Result<()> {
            let text = serde_json::to_string_pretty(asset)?;
            engine_fs::virtual_fs::write_file_atomically(path, text.as_bytes())?;
            Ok(())
        }
    };
}

asset_io!(BlendSpaceAsset, load_blend_space, save_blend_space);
asset_io!(AnimationMontageAsset, load_montage, save_montage);
asset_io!(
    AnimationGraphAsset,
    load_animation_graph,
    save_animation_graph
);
