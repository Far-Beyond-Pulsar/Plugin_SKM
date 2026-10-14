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
/// Every editor id this plugin provides.
pub const EDITOR_IDS: [&str; 6] = [
    SKELETON_EDITOR_ID,
    ANIM_SEQUENCE_EDITOR_ID,
    SKELETAL_MESH_EDITOR_ID,
    BLEND_SPACE_EDITOR_ID,
    ANIMATION_MONTAGE_EDITOR_ID,
    ANIMATION_GRAPH_EDITOR_ID,
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
        vec![
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
                id: FileTypeId::new(BLEND_SPACE_FILE_TYPE),
                extension: skeletal_assets::animation_sets::BLEND_SPACE_EXTENSION.to_string(),
                display_name: "Blend Space".to_string(),
                icon: ui::IconName::Keyframes,
                color: gpui::rgb(0x7E57C2).into(),
                structure: FileStructure::Standalone,
                default_content: serde_json::to_value(
                    skeletal_assets::animation_sets::BlendSpaceAsset::default(),
                )
                .unwrap_or_default(),
                categories: vec!["Animation".to_string()],
                creation_directory: None,
            },
            FileTypeDefinition {
                id: FileTypeId::new(ANIMATION_MONTAGE_FILE_TYPE),
                extension: skeletal_assets::animation_sets::MONTAGE_EXTENSION.to_string(),
                display_name: "Animation Montage".to_string(),
                icon: ui::IconName::Keyframes,
                color: gpui::rgb(0x26A69A).into(),
                structure: FileStructure::Standalone,
                default_content: serde_json::to_value(
                    skeletal_assets::animation_sets::AnimationMontageAsset::default(),
                )
                .unwrap_or_default(),
                categories: vec!["Animation".to_string()],
                creation_directory: None,
            },
            FileTypeDefinition {
                id: FileTypeId::new(ANIMATION_GRAPH_FILE_TYPE),
                extension: skeletal_assets::animation_sets::ANIMATION_GRAPH_EXTENSION.to_string(),
                display_name: "Animation Graph".to_string(),
                icon: ui::IconName::Keyframes,
                color: gpui::rgb(0xEF5350).into(),
                structure: FileStructure::Standalone,
                default_content: serde_json::to_value(
                    skeletal_assets::animation_sets::AnimationGraphAsset::default(),
                )
                .unwrap_or_default(),
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
        ]
    }

    fn editors(&self) -> Vec<EditorMetadata> {
        vec![
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
            EditorMetadata {
                id: EditorId::new(BLEND_SPACE_EDITOR_ID),
                display_name: "Blend Space Editor".into(),
                supported_file_types: vec![FileTypeId::new(BLEND_SPACE_FILE_TYPE)],
            },
            EditorMetadata {
                id: EditorId::new(ANIMATION_MONTAGE_EDITOR_ID),
                display_name: "Animation Montage Editor".into(),
                supported_file_types: vec![FileTypeId::new(ANIMATION_MONTAGE_FILE_TYPE)],
            },
            EditorMetadata {
                id: EditorId::new(ANIMATION_GRAPH_EDITOR_ID),
                display_name: "Animation Graph Editor".into(),
                supported_file_types: vec![FileTypeId::new(ANIMATION_GRAPH_FILE_TYPE)],
            },
        ]
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
            registry.register_fn(EditorId::new(id), |file_path, window, cx| {
                Self::create_editor(file_path, window, cx)
            });
        }
        for (id, kind) in [
            (
                BLEND_SPACE_EDITOR_ID,
                animation_assets_editor::AnimationAssetEditorKind::BlendSpace,
            ),
            (
                ANIMATION_MONTAGE_EDITOR_ID,
                animation_assets_editor::AnimationAssetEditorKind::Montage,
            ),
            (
                ANIMATION_GRAPH_EDITOR_ID,
                animation_assets_editor::AnimationAssetEditorKind::Graph,
            ),
        ] {
            registry.register_fn(EditorId::new(id), move |file_path, window, cx| {
                let panel = animation_assets_editor::AnimationAssetEditor::open(
                    file_path, kind, window, cx,
                )?;
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
        let panel = cx.new(|cx| {
            SkeletalAnimEditorPanel::new_with_path(file_path.clone(), window, cx).unwrap_or_else(
                |err| {
                    log::warn!("Failed to load animation asset {file_path:?}: {err}");
                    SkeletalAnimEditorPanel::new(window, cx)
                },
            )
        });
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
    fn every_asset_has_a_file_type_and_an_editor_and_only_the_first_two_can_be_created() {
        let plugin = SkeletalAnimationPlugin::default();
        let file_types = plugin.file_types();
        let editors = plugin.editors();

        for (file_type, extension, creatable) in [
            (SKELETON_FILE_TYPE, "skeleton", true),
            (ANIM_SEQUENCE_FILE_TYPE, "animseq", true),
            (SKELETAL_MESH_FILE_TYPE, "skmesh", false),
            (LEGACY_FILE_TYPE, "skelanim", false),
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
}
