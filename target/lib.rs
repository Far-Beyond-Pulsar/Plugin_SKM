#![recursion_limit = "256"]

//! # Skeletal Animation Editor Plugin
//!
//! Editor for authoring skeletal animation clips: a bone hierarchy tree, a
//! properties panel for the selected bone, a 3D viewport, and a keyframe
//! timeline. The viewport and timeline are both custom WGPU surfaces
//! composited directly into the GPUI scene.
//!
//! ## Architecture
//!
//! - **core**: Data model (Skeleton, Bone, AnimationClip, Transform/Mat4 math)
//! - **editor**: Main editor panel state, workspace/dock layout, operations
//! - **rendering**: WGPU renderers + dock panels for the viewport and timeline
//! - **ui_components**: Bone hierarchy tree and bone properties dock panels

use gpui::*;
use plugin_editor_api::*;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Mutex;
use std::{path::PathBuf, sync::Arc};
use ui::dock::PanelView;

mod core;
mod editor;
mod rendering;
mod ui_components;

pub use core::*;
pub use editor::panel::SkeletalAnimEditorPanel;

/// Storage for editor instances owned by the plugin.
struct EditorStorage {
    panel: Arc<dyn ui::dock::PanelView>,
}

/// The Skeletal Animation Editor Plugin.
pub struct SkeletalAnimationPlugin {
    /// CRITICAL: Plugin owns ALL editor instances to prevent memory leaks!
    /// The main app only gets raw pointers - it NEVER owns the Arc or Box.
    editors: Arc<Mutex<HashMap<usize, EditorStorage>>>,
    next_editor_id: Arc<Mutex<usize>>,
}

impl Default for SkeletalAnimationPlugin {
    fn default() -> Self {
        Self {
            editors: Arc::new(Mutex::new(HashMap::new())),
            next_editor_id: Arc::new(Mutex::new(0)),
        }
    }
}

impl EditorPlugin for SkeletalAnimationPlugin {
    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            id: PluginId::new("com.pulsar.skeletal-animation-editor"),
            name: "Skeletal Animation Editor".into(),
            version: "0.1.0".into(),
            author: "Pulsar Team".into(),
            description: "Editor for skeletal mesh animation clips".into(),
        }
    }

    fn file_types(&self) -> Vec<FileTypeDefinition> {
        let sample_skeleton = core::sample::humanoid_skeleton();
        let sample_animation = core::sample::idle_animation(&sample_skeleton);

        vec![FileTypeDefinition {
            id: FileTypeId::new("skelanim"),
            extension: "skelanim".to_string(),
            display_name: "Skeletal Animation".to_string(),
            icon: ui::IconName::Play,
            color: gpui::rgb(0x4FC3F7).into(),
            structure: FileStructure::FolderBased {
                marker_file: core::serialization::SKELETON_FILE.to_string(),
                template_structure: vec![PathTemplate::File {
                    path: core::serialization::ANIMATION_FILE.to_string(),
                    content: serde_json::to_string_pretty(&sample_animation)
                        .unwrap_or_default(),
                }],
            },
            default_content: serde_json::to_value(&sample_skeleton).unwrap_or(json!({})),
            categories: vec!["Animation".to_string()],
        }]
    }

    fn editors(&self) -> Vec<EditorMetadata> {
        vec![EditorMetadata {
            id: EditorId::new("skeletal-animation-editor"),
            display_name: "Skeletal Animation Editor".into(),
            supported_file_types: vec![FileTypeId::new("skelanim")],
        }]
    }

    fn create_editor(
        &self,
        editor_id: EditorId,
        file_path: PathBuf,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<Arc<dyn PanelView>, PluginError> {
        log::info!(
            "Creating skeletal animation editor with ID: {}",
            editor_id.as_str()
        );

        if editor_id.as_str() != "skeletal-animation-editor" {
            return Err(PluginError::EditorNotFound { editor_id });
        }

        let panel = cx.new(|cx| {
            match SkeletalAnimEditorPanel::new_with_path(file_path.clone(), window, cx) {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(">>> create_editor: new_with_path failed: {}", e);
                    SkeletalAnimEditorPanel::new(window, cx)
                }
            }
        });

        let panel_arc: Arc<dyn ui::dock::PanelView> = Arc::new(panel.clone());

        let id = {
            let mut next_id = self.next_editor_id.lock().unwrap();
            let id = *next_id;
            *next_id += 1;
            id
        };

        self.editors.lock().unwrap().insert(
            id,
            EditorStorage {
                panel: panel_arc.clone(),
            },
        );

        log::info!(
            "Created skeletal animation editor instance {} for {:?}",
            id,
            file_path
        );

        Ok(panel_arc)
    }

    fn on_load(&mut self) {
        log::info!("Skeletal Animation Editor Plugin loaded");
    }
}
