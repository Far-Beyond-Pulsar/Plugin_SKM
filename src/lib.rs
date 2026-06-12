#![recursion_limit = "512"]

//! # Skeletal Animation Editor Plugin
//!
//! Editor for authoring skeletal animation clips: a bone hierarchy tree, a
//! 3D viewport, bone transform properties, and a keyframe timeline.
//!
//! ## Architecture
//!
//! - **core**: Data model (skeleton, animation clips, math, sample data, serialization)
//! - **editor**: Top-level editor panel, transform inputs, and dock workspace layout
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
mod ui_components;

pub use editor::SkeletalAnimEditorPanel;

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
            description: "Editor for authoring skeletal animation clips".into(),
        }
    }

    fn file_types(&self) -> Vec<FileTypeDefinition> {
        vec![FileTypeDefinition {
            id: FileTypeId::new("skeletal_animation"),
            extension: "skelanim".to_string(),
            display_name: "Skeletal Animation".to_string(),
            icon: ui::IconName::Component,
            color: gpui::rgb(0x2196F3).into(),
            structure: FileStructure::FolderBased {
                marker_file: "skeleton.json".to_string(),
                template_structure: vec![],
            },
            default_content: json!({}),
            categories: vec!["Animation".to_string()],
        }]
    }

    fn editors(&self) -> Vec<EditorMetadata> {
        vec![EditorMetadata {
            id: EditorId::new("skeletal-animation-editor"),
            display_name: "Skeletal Animation Editor".into(),
            supported_file_types: vec![FileTypeId::new("skeletal_animation")],
        }]
    }

    fn create_editor(
        &self,
        editor_id: EditorId,
        file_path: PathBuf,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<Arc<dyn PanelView>, PluginError> {
        if editor_id.as_str() != "skeletal-animation-editor" {
            return Err(PluginError::EditorNotFound { editor_id });
        }

        let panel = cx.new(|cx| {
            SkeletalAnimEditorPanel::new_with_path(file_path.clone(), window, cx)
                .unwrap_or_else(|err| {
                    log::warn!("Failed to load skeletal animation asset {file_path:?}: {err}");
                    SkeletalAnimEditorPanel::new(window, cx)
                })
        });

        Ok(Arc::new(panel) as Arc<dyn PanelView>)
    }

    fn on_load(&mut self) {
        log::info!("Skeletal Animation Editor Plugin loaded");
    }
}

// export_plugin!(SkeletalAnimationPlugin);
