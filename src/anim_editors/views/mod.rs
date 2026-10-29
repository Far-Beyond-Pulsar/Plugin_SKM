//! GPUI views of the animation asset editors. Each view renders a model from
//! `anim_editors` and turns input into model commands; none holds authored
//! state of its own.

pub mod chrome;
pub mod anim_graph_canvas;
pub mod anim_graph_view;
pub mod blend_space_view;
pub mod ik_rig_view;
pub mod montage_view;
pub mod notify_inspector;
pub mod paint;
pub mod pose_db_view;
pub mod retarget_view;
pub mod skeleton_canvas;
