//! Production editors for the animation asset family (Phase 4).
//!
//! Every editor splits in two:
//!
//! - a **model** (the `*_model.rs` files): the typed document, its commands,
//!   selection, derived data and validation. Models are plain Rust, tested
//!   headless, and are the only place assets are edited.
//! - a **view** (`views/`): GPUI panels that render a model and turn input into
//!   model commands. A view holds no authored state of its own.
//!
//! Shared pieces: [`document::AnimDocument`] (typed value, undo/redo, dirty,
//! conflict-checked atomic save), [`selection::AnimSelection`] (the typed
//! selection the inspector renders), [`store::DocStore`] and
//! the host undo history (`plugin_editor_api::undo`).

pub mod blend_space_model;
pub mod views;
pub mod document;
pub mod ik_rig_model;
pub mod montage_model;
pub mod notify_edit;
pub mod pose_db_model;
pub mod retarget_model;
pub mod refs;
pub mod selection;
pub mod store;
