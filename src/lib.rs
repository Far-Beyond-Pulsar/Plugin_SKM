//! # Skeletal Animation Editor Plugin
//!
//! Editors for two linked animation assets, in the Unreal style:
//!
//! - **Skeleton** (`.skeleton`): the bone hierarchy and bind pose, with its own
//!   stable id. Edited with a hierarchy tree, a 3D viewport and bone transform
//!   properties.
//! - **Animation Sequence** (`.animseq`): tracks and keyframes over time. It
//!   refers to its skeleton by that id, never by path, and adds a keyframe
//!   timeline to the same panels.
//!
//! Old `.skelanim` folders (skeleton and clip in one asset) are converted to a
//! pair of the new assets when opened; the folder is left in place.
//!
//! ## Architecture
//!
//! - **core**: Data model (skeleton, animation clips, math, sample data), the
//!   asset formats, the skeleton id and project library, validation, migration
//! - **editor**: Top-level editor panel, transform inputs, dock workspace
//!   layout, and the sequence's skeleton reference
//! - **rendering**: Custom WGPU-rendered viewport and timeline panels
//! - **ui_components**: Bone hierarchy and properties dock panels

use std::path::PathBuf;
use std::sync::Arc;

use gpui::*;
use plugin_editor_api::*;
use serde_json::json;
use ui::dock::PanelView;

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
/// Every editor id this plugin provides.
pub const EDITOR_IDS: [&str; 2] = [SKELETON_EDITOR_ID, ANIM_SEQUENCE_EDITOR_ID];

/// File type of `.skeleton` assets.
pub const SKELETON_FILE_TYPE: &str = "skeleton";
/// File type of `.animseq` assets.
pub const ANIM_SEQUENCE_FILE_TYPE: &str = "animation_sequence";
/// File type of the old `.skelanim` folders. Kept so they still open (and
/// convert); it is never offered in the create menu.
pub const LEGACY_FILE_TYPE: &str = "skeletal_animation";

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
            description: "Editors for skeletons and the animation sequences that use them".into(),
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
        ]
    }

    fn on_load(&mut self) {
        log::info!("Skeletal Animation Editor Plugin loaded");
    }
}

impl EditorPluginEditor for SkeletalAnimationPlugin {
    fn register_editors(&'static self, registry: &mut EditorFactoryRegistry) {
        // Both editors are the same panel; the file decides which mode it opens in.
        for id in EDITOR_IDS {
            registry.register_fn(EditorId::new(id), |file_path, window, cx| {
                Self::create_editor(file_path, window, cx)
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
