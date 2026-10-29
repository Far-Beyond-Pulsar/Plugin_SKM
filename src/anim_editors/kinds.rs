//! The animation asset kinds: ids, titles, extensions, the content of a new
//! file, and the one place an editor for a kind is opened.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::*;
use plugin_editor_api::{PluginError, UndoService};
use pulsar_reflection::{
    AnimationGraphAssetKind, AnimationMontageAssetKind, AssetKind, BlendSpaceAssetKind, IkRetargeterAssetKind, IkRigAssetKind, PoseDatabaseAssetKind, RetargetProfileAssetKind,
};
use serde_json::Value;
use skeletal_assets::anim_graph::AnimationGraphAsset;
use skeletal_assets::blend_space::BlendSpaceAsset;
use skeletal_assets::ik_retarget::IkRetargetAsset;
use skeletal_assets::ik_rig::IkRigAsset;
use skeletal_assets::montage::AnimationMontageAsset;
use skeletal_assets::pose_db::PoseDbAsset;
use skeletal_assets::retarget::RetargetProfileAsset;
use ui::dock::PanelView;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimationAssetEditorKind {
    BlendSpace,
    Montage,
    Graph,
    RetargetProfile,
    IkRig,
    IkRetarget,
    PoseDb,
}

impl AnimationAssetEditorKind {
    pub const ALL: [Self; 7] = [
        Self::BlendSpace,
        Self::Montage,
        Self::Graph,
        Self::RetargetProfile,
        Self::IkRig,
        Self::IkRetarget,
        Self::PoseDb,
    ];

    pub fn editor_id(self) -> &'static str {
        match self {
            Self::BlendSpace => "blend-space-editor",
            Self::Montage => "animation-montage-editor",
            Self::Graph => "animation-graph-editor",
            Self::RetargetProfile => "retarget-profile-editor",
            Self::IkRig => "ik-rig-editor",
            Self::IkRetarget => "ik-retargeter-editor",
            Self::PoseDb => "pose-search-database-editor",
        }
    }
    pub fn file_type(self) -> &'static str {
        match self {
            Self::BlendSpace => "blend_space",
            Self::Montage => "animation_montage",
            Self::Graph => "animation_graph",
            Self::RetargetProfile => "retarget_profile",
            Self::IkRig => "ik_rig",
            Self::IkRetarget => "ik_retargeter",
            Self::PoseDb => "pose_database",
        }
    }
    pub fn extension(self) -> &'static str {
        match self {
            Self::BlendSpace => skeletal_assets::blend_space::BLEND_SPACE_EXTENSION,
            Self::Montage => skeletal_assets::montage::MONTAGE_EXTENSION,
            Self::Graph => skeletal_assets::anim_graph::ANIMATION_GRAPH_EXTENSION,
            Self::RetargetProfile => skeletal_assets::retarget::RETARGET_EXTENSION,
            Self::IkRig => skeletal_assets::ik_rig::IK_RIG_EXTENSION,
            Self::IkRetarget => skeletal_assets::ik_retarget::IK_RETARGET_EXTENSION,
            Self::PoseDb => skeletal_assets::pose_db::POSE_DB_EXTENSION,
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            Self::BlendSpace => "Blend Space",
            Self::Montage => "Animation Montage",
            Self::Graph => "Animation Graph",
            Self::RetargetProfile => "Retarget Profile",
            Self::IkRig => "IK Rig",
            Self::IkRetarget => "IK Retargeter",
            Self::PoseDb => "Pose Search Database",
        }
    }
    pub fn asset_kind(self) -> &'static str {
        match self {
            Self::BlendSpace => BlendSpaceAssetKind::KIND,
            Self::Montage => AnimationMontageAssetKind::KIND,
            Self::Graph => AnimationGraphAssetKind::KIND,
            Self::RetargetProfile => RetargetProfileAssetKind::KIND,
            Self::IkRig => IkRigAssetKind::KIND,
            Self::IkRetarget => IkRetargeterAssetKind::KIND,
            Self::PoseDb => PoseDatabaseAssetKind::KIND,
        }
    }

    /// The text of a new, valid, empty asset of this kind. Used as the create
    /// menu's default content and when a file is opened before it has any.
    pub fn default_text(self) -> String {
        match self {
            Self::BlendSpace => BlendSpaceAsset::default().to_text(),
            Self::Montage => AnimationMontageAsset::default().to_text(),
            Self::Graph => AnimationGraphAsset::default().to_text(),
            Self::RetargetProfile => RetargetProfileAsset::new("New Retarget Profile", None, None).to_text(),
            Self::IkRig => IkRigAsset::new("New IK Rig", None).to_text(),
            Self::IkRetarget => IkRetargetAsset::new("New IK Retargeter", None, None).to_text(),
            Self::PoseDb => PoseDbAsset::new("New Pose Database", None).to_text(),
        }
        .unwrap_or_default()
    }

    /// The default content as JSON, the form file type definitions take.
    pub fn default_json(self) -> Value {
        serde_json::from_str(&self.default_text()).unwrap_or(Value::Null)
    }
}


impl AnimationAssetEditorKind {
    /// Open an editor for the file at `path`. Each kind has its own purpose-built
    /// editor; with the host's undo service, edits join the document's shared history.
    pub fn open(self, path: PathBuf, undo: Option<&UndoService>, window: &mut Window, cx: &mut App) -> Result<Arc<dyn PanelView>, PluginError> {
        use super::views::{
            anim_graph_view::AnimGraphEditor, blend_space_view::BlendSpaceEditor, ik_rig_view::IkRigEditor, montage_view::MontageEditor, pose_db_view::PoseDbEditor,
            retarget_view::{IkRetargetEditor, RetargetProfileEditor},
        };
        macro_rules! open {
            ($ty:ty) => {{
                let entity = <$ty>::open(path, window, cx)?;
                if let Some(undo) = undo {
                    entity.update(cx, |editor, _| editor.adopt_undo(undo));
                }
                Ok(Arc::new(entity) as Arc<dyn PanelView>)
            }};
        }
        match self {
            Self::BlendSpace => open!(BlendSpaceEditor),
            Self::Montage => open!(MontageEditor),
            Self::Graph => open!(AnimGraphEditor),
            Self::RetargetProfile => open!(RetargetProfileEditor),
            Self::IkRig => open!(IkRigEditor),
            Self::IkRetarget => open!(IkRetargetEditor),
            Self::PoseDb => open!(PoseDbEditor),
        }
    }

    /// Check that `text` loads as this kind (the registration tests use it).
    #[cfg(test)]
    pub fn parses(self, text: &str) -> Result<String, String> {
        use super::document::AnimAsset;
        match self {
            Self::BlendSpace => BlendSpaceAsset::parse_text(text).map(|(a, _)| a.asset_id()),
            Self::Montage => AnimationMontageAsset::parse_text(text).map(|(a, _)| a.asset_id()),
            Self::Graph => AnimationGraphAsset::parse_text(text).map(|(a, _)| a.asset_id()),
            Self::RetargetProfile => RetargetProfileAsset::parse_text(text).map(|(a, _)| a.asset_id()),
            Self::IkRig => IkRigAsset::parse_text(text).map(|(a, _)| a.asset_id()),
            Self::IkRetarget => IkRetargetAsset::parse_text(text).map(|(a, _)| a.asset_id()),
            Self::PoseDb => PoseDbAsset::parse_text(text).map(|(a, _)| a.asset_id()),
        }
    }
}
