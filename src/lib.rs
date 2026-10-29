//! # Skeletal Animation Editor Plugin
//!
//! Editors for three linked animation assets, in the Unreal style:
//!
//! - **Skeleton** (`.skeleton`): the bone hierarchy and bind pose, with its own
//!   stable id. Edited with a hierarchy tree, a 3D viewport and bone transform
//!   properties.
//! - **Animation Sequence** (`.animseq`): tracks and keyframes over time. It
//!   refers to its skeleton by that id, never by path, and adds a keyframe
//!   timeline to the same panels.
//! - **Skeletal Mesh** (`.skmesh`): skinned geometry (per-vertex joints and
//!   weights) bound to a skeleton by id, with its own id. The editor draws it
//!   skinned to the posed skeleton and inspects its weights, joints, material
//!   slots and statistics. It is made by importing a rigged model
//!   (`asset_import`), not created from the menu.
//!
//! A skeleton or a sequence can name a skeletal mesh (by id) to draw with it.
//!
//! Old `.skelanim` folders (skeleton and clip in one asset) are converted to a
//! pair of the new assets when opened; the folder is left in place.
//!
//! ## Architecture
//!
//! - **core**: The project libraries (skeletons and meshes by id), validation,
//!   migration and sample data. The asset formats, ids, math and skinning are
//!   in the `skeletal_assets` crate, which the importer shares
//! - **editor**: Top-level editor panel, transform inputs, dock workspace
//!   layout, and the skeleton and preview-mesh references
//! - **rendering**: Custom WGPU-rendered viewport and timeline panels
//! - **ui_components**: Bone hierarchy and properties dock panels

use std::path::PathBuf;
use std::sync::Arc;

use gpui::*;
use plugin_editor_api::*;
use serde_json::json;
use ui::dock::PanelView;

mod animation_assets_editor;
mod core;
mod editor;
mod rendering;
mod style;
mod ui_components;

pub use editor::{EditorMode, SkeletalAnimEditorPanel};

/// Id of the Skeleton editor.
pub const SKELETON_EDITOR_ID: &str = "skeleton-editor";
/// Id of the Animation Sequence editor (which also opens old `.skelanim` folders).
pub const ANIM_SEQUENCE_EDITOR_ID: &str = "animation-sequence-editor";
/// Id of the Skeletal Mesh editor.
pub const SKELETAL_MESH_EDITOR_ID: &str = "skeletal-mesh-editor";
pub const BLEND_SPACE_EDITOR_ID: &str = "blend-space-editor";
pub const ANIMATION_MONTAGE_EDITOR_ID: &str = "animation-montage-editor";
pub const ANIMATION_GRAPH_EDITOR_ID: &str = "animation-graph-editor";
pub const RETARGET_PROFILE_EDITOR_ID: &str = "retarget-profile-editor";
pub const IK_RIG_EDITOR_ID: &str = "ik-rig-editor";
pub const IK_RETARGETER_EDITOR_ID: &str = "ik-retargeter-editor";
pub const POSE_DATABASE_EDITOR_ID: &str = "pose-search-database-editor";
/// Every editor id this plugin provides.
pub const EDITOR_IDS: [&str; 10] = [
    SKELETON_EDITOR_ID,
    ANIM_SEQUENCE_EDITOR_ID,
    SKELETAL_MESH_EDITOR_ID,
    BLEND_SPACE_EDITOR_ID,
    ANIMATION_MONTAGE_EDITOR_ID,
    ANIMATION_GRAPH_EDITOR_ID,
    RETARGET_PROFILE_EDITOR_ID,
    IK_RIG_EDITOR_ID,
    IK_RETARGETER_EDITOR_ID,
    POSE_DATABASE_EDITOR_ID,
];

/// File type of `.skeleton` assets.
pub const SKELETON_FILE_TYPE: &str = "skeleton";
/// File type of `.animseq` assets.
pub const ANIM_SEQUENCE_FILE_TYPE: &str = "animation_sequence";
/// File type of `.skmesh` assets.
pub const SKELETAL_MESH_FILE_TYPE: &str = "skeletal_mesh";
/// File type of the old `.skelanim` folders. Kept so they still open (and
/// convert); it is never offered in the create menu.
pub const LEGACY_FILE_TYPE: &str = "skeletal_animation";
pub const BLEND_SPACE_FILE_TYPE: &str = "blend_space";
pub const ANIMATION_MONTAGE_FILE_TYPE: &str = "animation_montage";
pub const ANIMATION_GRAPH_FILE_TYPE: &str = "animation_graph";
pub const RETARGET_PROFILE_FILE_TYPE: &str = "retarget_profile";
pub const IK_RIG_FILE_TYPE: &str = "ik_rig";
pub const IK_RETARGETER_FILE_TYPE: &str = "ik_retargeter";
pub const POSE_DATABASE_FILE_TYPE: &str = "pose_database";

/// The create-menu entry of one high-level animation asset. Its default
/// content is a valid empty document written by the real codec, so a new file
/// opens without repair.
fn animation_asset_file_type(kind: animation_assets_editor::AnimationAssetEditorKind) -> FileTypeDefinition {
    use animation_assets_editor::AnimationAssetEditorKind as K;
    let color = match kind {
        K::BlendSpace => 0x7E57C2,
        K::Montage => 0x26A69A,
        K::Graph => 0xEF5350,
        K::RetargetProfile => 0xFFA726,
        K::IkRig => 0x66BB6A,
        K::IkRetarget => 0x29B6F6,
        K::PoseDb => 0xEC407A,
    };
    FileTypeDefinition {
        id: FileTypeId::new(kind.file_type()),
        extension: kind.extension().to_string(),
        display_name: kind.title().to_string(),
        icon: ui::IconName::Keyframes,
        color: gpui::rgb(color).into(),
        structure: FileStructure::Standalone,
        default_content: kind.default_json(),
        categories: vec!["Animation".to_string()],
        creation_directory: None,
    }
}

/// Whether `editor_id` is one of this plugin's editors.
pub fn provides_editor(editor_id: &str) -> bool {
    EDITOR_IDS.contains(&editor_id)
}

/// The Skeletal Animation Editor Plugin.
#[derive(Default)]
pub struct SkeletalAnimationPlugin;

impl EditorPlugin for SkeletalAnimationPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: PluginId::new("com.pulsar.skeletal-animation-editor"),
            name: "Skeletal Animation Editor".into(),
            version: "0.1.0".into(),
            author: "Pulsar Team".into(),
            description:
                "Editors for skeletons, skeletal meshes and the animation sequences that use them"
                    .into(),
        }
    }

    fn file_types(&self) -> Vec<FileTypeDefinition> {
        let mut types = vec![
            FileTypeDefinition {
                id: FileTypeId::new(SKELETON_FILE_TYPE),
                extension: core::assets::SKELETON_EXTENSION.to_string(),
                display_name: "Skeleton".to_string(),
                icon: ui::IconName::Accessibility,
                color: gpui::rgb(0xFF9800).into(),
                structure: FileStructure::Standalone,
                // The id is written when the editor first opens the file.
                default_content: json!({
                    "version": core::assets::FORMAT_VERSION,
                    "bones": [{
                        "id": "root",
                        "name": "Root",
                        "parent": null,
                        "bind_transform": core::Transform::IDENTITY,
                    }],
                }),
                categories: vec!["Animation".to_string()],
                creation_directory: None,
            },
            FileTypeDefinition {
                id: FileTypeId::new(ANIM_SEQUENCE_FILE_TYPE),
                extension: core::assets::ANIMSEQ_EXTENSION.to_string(),
                display_name: "Animation Sequence".to_string(),
                icon: ui::IconName::Keyframes,
                color: gpui::rgb(0x2196F3).into(),
                structure: FileStructure::Standalone,
                default_content: json!({
                    "version": core::assets::FORMAT_VERSION,
                    "skeleton": null,
                    "name": "New Animation",
                    "duration": 1.0,
                    "fps": 30.0,
                    "tracks": [],
                }),
                categories: vec!["Animation".to_string()],
                creation_directory: None,
            },
            FileTypeDefinition {
                id: FileTypeId::new(SKELETAL_MESH_FILE_TYPE),
                extension: core::skmesh::SKMESH_EXTENSION.to_string(),
                display_name: "Skeletal Mesh".to_string(),
                icon: ui::IconName::Cube,
                color: gpui::rgb(0x4CAF50).into(),
                structure: FileStructure::Standalone,
                // Made by importing a rigged model; there is nothing to create
                // from the menu (the empty string hides the type there).
                default_content: json!(""),
                categories: vec!["Animation".to_string()],
                creation_directory: None,
            },
            FileTypeDefinition {
                id: FileTypeId::new(LEGACY_FILE_TYPE),
                extension: "skelanim".to_string(),
                display_name: "Skeletal Animation (old)".to_string(),
                icon: ui::IconName::Component,
                color: gpui::rgb(0x9E9E9E).into(),
                structure: FileStructure::FolderBased {
                    marker_file: "skeleton.json".to_string(),
                    template_structure: vec![],
                },
                // Existing folders only; new ones are made as the two assets.
                default_content: json!(""),
                categories: vec!["Animation".to_string()],
                creation_directory: None,
            },
        ];
        types.extend(animation_assets_editor::AnimationAssetEditorKind::ALL.map(animation_asset_file_type));
        types
    }

    fn editors(&self) -> Vec<EditorMetadata> {
        let mut editors = vec![
            EditorMetadata {
                id: EditorId::new(SKELETON_EDITOR_ID),
                display_name: "Skeleton Editor".into(),
                supported_file_types: vec![FileTypeId::new(SKELETON_FILE_TYPE)],
            },
            EditorMetadata {
                id: EditorId::new(ANIM_SEQUENCE_EDITOR_ID),
                display_name: "Animation Sequence Editor".into(),
                supported_file_types: vec![
                    FileTypeId::new(ANIM_SEQUENCE_FILE_TYPE),
                    FileTypeId::new(LEGACY_FILE_TYPE),
                ],
            },
            EditorMetadata {
                id: EditorId::new(SKELETAL_MESH_EDITOR_ID),
                display_name: "Skeletal Mesh Editor".into(),
                supported_file_types: vec![FileTypeId::new(SKELETAL_MESH_FILE_TYPE)],
            },
        ];
        editors.extend(animation_assets_editor::AnimationAssetEditorKind::ALL.map(|kind| EditorMetadata {
            id: EditorId::new(kind.editor_id()),
            display_name: format!("{} Editor", kind.title()).into(),
            supported_file_types: vec![FileTypeId::new(kind.file_type())],
        }));
        editors
    }

    fn on_load(&mut self) {
        log::info!("Skeletal Animation Editor Plugin loaded");
    }
}

impl EditorPluginEditor for SkeletalAnimationPlugin {
    fn register_editors(&'static self, registry: &mut EditorFactoryRegistry) {
        // Both editors are the same panel; the file decides which mode it opens in.
        for id in [
            SKELETON_EDITOR_ID,
            ANIM_SEQUENCE_EDITOR_ID,
            SKELETAL_MESH_EDITOR_ID,
        ] {
            registry.register_fn_with_settings(
                EditorId::new(id),
                |file_path, settings, window, cx| {
                    Self::create_editor_with_undo(
                        file_path,
                        Some(&settings.services().undo),
                        window,
                        cx,
                    )
                },
            );
        }
        for kind in animation_assets_editor::AnimationAssetEditorKind::ALL {
            registry.register_fn(EditorId::new(kind.editor_id()), move |file_path, window, cx| {
                let panel = animation_assets_editor::AnimationAssetEditor::open(file_path, kind, window, cx)?;
                Ok(Arc::new(panel) as Arc<dyn PanelView>)
            });
        }
    }
}

impl EditorPluginStatusbar for SkeletalAnimationPlugin {}
impl EditorPluginAi for SkeletalAnimationPlugin {}
impl EditorPluginComponents for SkeletalAnimationPlugin {
    fn component_definitions(&self) -> Vec<ComponentDefinition> {
        Vec::new()
    }
}
impl EditorPluginSubsystems for SkeletalAnimationPlugin {
    fn subsystems(&self) -> Vec<Box<dyn Subsystem>> {
        Vec::new()
    }
}

impl SkeletalAnimationPlugin {
    /// Open `file_path` in a new editor panel (for hosts that link this crate).
    /// A `.skeleton` opens the Skeleton editor, an `.animseq` or an old
    /// `.skelanim` folder the Animation Sequence editor.
    pub fn create_editor(
        file_path: PathBuf,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<Arc<dyn PanelView>, PluginError> {
        Self::create_editor_with_undo(file_path, None, window, cx)
    }

    /// open an editor with the host's per-document undo service.
    pub fn create_editor_with_undo(
        file_path: PathBuf,
        undo: Option<&UndoService>,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<Arc<dyn PanelView>, PluginError> {
        let mut load_error = None;
        let panel = cx.new(|cx| {
            SkeletalAnimEditorPanel::new_with_path(file_path.clone(), window, cx).unwrap_or_else(
                |err| {
                    load_error = Some(err.to_string());
                    SkeletalAnimEditorPanel::new(window, cx)
                },
            )
        });
        if let Some(message) = load_error {
            return Err(PluginError::Other { message });
        }
        if let Some(undo) = undo {
            panel.update(cx, |panel, _| panel.set_undo_service(undo));
        }
        Ok(Arc::new(panel) as Arc<dyn PanelView>)
    }
}

// Static built-ins are instantiated through the Rust API and must not emit the
// process-global dynamic loader symbols shared by every plugin.
#[cfg(not(feature = "builtin"))]
export_plugin!(SkeletalAnimationPlugin);

#[cfg(test)]
mod tests {
    // Not `use super::*`: it would bring in gpui's `test` attribute macro.
    use super::{
        provides_editor, SkeletalAnimationPlugin, ANIM_SEQUENCE_FILE_TYPE, EDITOR_IDS,
        LEGACY_FILE_TYPE, SKELETAL_MESH_FILE_TYPE, SKELETON_FILE_TYPE,
    };
    use plugin_editor_api::EditorPlugin;

    #[test]
    fn every_asset_has_a_file_type_and_an_editor_and_only_imports_and_legacy_cannot_be_created() {
        let plugin = SkeletalAnimationPlugin::default();
        let file_types = plugin.file_types();
        let editors = plugin.editors();

        for (file_type, extension, creatable) in [
            (SKELETON_FILE_TYPE, "skeleton", true),
            (ANIM_SEQUENCE_FILE_TYPE, "animseq", true),
            (SKELETAL_MESH_FILE_TYPE, "skmesh", false),
            (LEGACY_FILE_TYPE, "skelanim", false),
            (super::BLEND_SPACE_FILE_TYPE, "blendspace", true),
            (super::ANIMATION_MONTAGE_FILE_TYPE, "montage", true),
            (super::ANIMATION_GRAPH_FILE_TYPE, "animgraph", true),
            (super::RETARGET_PROFILE_FILE_TYPE, "animretarget", true),
            (super::IK_RIG_FILE_TYPE, "ikrig", true),
            (super::IK_RETARGETER_FILE_TYPE, "ikretarget", true),
            (super::POSE_DATABASE_FILE_TYPE, "posedb", true),
        ] {
            let definition = file_types
                .iter()
                .find(|d| d.id.as_str() == file_type)
                .unwrap_or_else(|| panic!("no file type {file_type}"));
            assert_eq!(definition.extension, extension);
            assert_eq!(definition.is_creatable(), creatable, "{file_type}");
            assert!(
                editors.iter().any(|e| e
                    .supported_file_types
                    .iter()
                    .any(|t| t.as_str() == file_type)),
                "{file_type} has no editor"
            );
        }

        for id in EDITOR_IDS {
            assert!(provides_editor(id));
            assert!(
                editors.iter().any(|e| e.id.as_str() == id),
                "{id} is not listed"
            );
        }
        assert!(!provides_editor("something-else"));
    }

    #[test]
    fn every_high_level_asset_is_registered_end_to_end_with_a_valid_default() {
        use crate::animation_assets_editor::AnimationAssetEditorKind as K;
        use pulsar_reflection::AssetKind;
        let plugin = SkeletalAnimationPlugin::default();
        let file_types = plugin.file_types();
        let editors = plugin.editors();
        let mut extensions = std::collections::HashSet::new();
        for kind in K::ALL {
            let definition = file_types
                .iter()
                .find(|d| d.id.as_str() == kind.file_type())
                .unwrap_or_else(|| panic!("no file type for {}", kind.title()));
            assert_eq!(definition.extension, kind.extension());
            assert!(extensions.insert(definition.extension.clone()), "one extension per format");
            assert!(definition.is_creatable(), "{} is in the create menu", kind.title());
            assert!(definition.categories.iter().any(|c| c == "Animation"));
            assert!(provides_editor(kind.editor_id()));
            assert!(editors.iter().any(|e| e.id.as_str() == kind.editor_id()
                && e.supported_file_types.iter().any(|t| t.as_str() == kind.file_type())));
            // the menu writes this content as the new file; the real codec must accept it
            let text = definition.default_content.to_string();
            let (document, _) = crate::animation_assets_editor::parse_for_test(kind, &text)
                .unwrap_or_else(|e| panic!("{} default content does not load: {e}", kind.title()));
            assert!(!document.is_empty());
            // and it indexes under the kind the picker and Reflection use
            assert_eq!(skeletal_assets::dependencies::kind_of_extension(kind.extension()), Some(kind.asset_kind()));
        }
        assert_eq!(K::ALL.len(), 7);
        assert_eq!(skeletal_assets::dependencies::extension_of(pulsar_reflection::AnimationMontageAssetKind::KIND), Some("montage"));
    }
}
