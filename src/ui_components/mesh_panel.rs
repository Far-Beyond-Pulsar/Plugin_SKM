//! Right dock panel of the skeletal mesh editor: mesh statistics, material
//! slots, and the joint table that maps the mesh's joints to the skeleton's
//! bones, with the weights of the selected bone.
//!
//! Selecting a joint here selects its bone, which colors the mesh in the
//! viewport by that bone's influence.

use gpui::prelude::FluentBuilder;
use gpui::*;
use ui::button::{Button, ButtonVariants};
use ui::dock::PanelEvent;
use ui::popover::Popover;
use ui::scroll::ScrollbarAxis;
use ui::tag::Tag;
use ui::{h_flex, v_flex, ActiveTheme, Icon, IconName, Sizable, StyledExt};
use ui_common::MeshAssetPicker;

use crate::core::skinning::{bone_influence, joint_vertex_counts};
use crate::core::skmesh::{MeshStats, SkeletalMeshAsset};
use crate::core::validate::{Issue, Severity};
use crate::core::Skeleton;
use crate::editor::SkeletalAnimEditorPanel;
use crate::ui_components::chrome::{empty_message, panel_header, MONO};

pub struct MeshInspectorPanel {
    editor: WeakEntity<SkeletalAnimEditorPanel>,
    focus_handle: FocusHandle,
}

/// One row of the joint table.
#[derive(Clone, Debug, PartialEq)]
pub struct JointRow {
    pub index: usize,
    /// The bone's id, as the mesh stores it.
    pub bone: String,
    /// The bone's display name, or its id if the skeleton has none like it.
    pub name: String,
    /// The skeleton has no bone with this id (or there is no skeleton yet).
    pub missing: bool,
    /// Vertices the joint has any influence on.
    pub vertices: usize,
}

/// The joint table for `mesh` against `skeleton`. `skeleton_known` says whether
/// a skeleton resolved at all; without one no joint is called missing.
pub fn joint_rows(
    mesh: &SkeletalMeshAsset,
    skeleton: &Skeleton,
    skeleton_known: bool,
) -> Vec<JointRow> {
    let counts = mesh
        .lod0()
        .map(|lod| joint_vertex_counts(mesh, lod))
        .unwrap_or_default();
    mesh.joints
        .iter()
        .enumerate()
        .map(|(index, joint)| {
            let bone = skeleton.bone(&joint.bone);
            JointRow {
                index,
                bone: joint.bone.clone(),
                name: bone.map_or_else(|| joint.bone.clone(), |b| b.name.clone()),
                missing: skeleton_known && bone.is_none(),
                vertices: counts.get(index).copied().unwrap_or(0),
            }
        })
        .collect()
}

/// "none", or how many errors and warnings `issues` holds.
pub fn issue_summary(issues: &[Issue]) -> String {
    let count = |severity| issues.iter().filter(|i| i.severity == severity).count();
    match (count(Severity::Error), count(Severity::Warning)) {
        (0, 0) => "none".to_string(),
        (errors, 0) => format!("{errors} error(s)"),
        (0, warnings) => format!("{warnings} warning(s)"),
        (errors, warnings) => format!("{errors} error(s), {warnings} warning(s)"),
    }
}

/// How many vertices `bone` influences and the strongest weight it has, over
/// the first LOD.
pub fn bone_weight_summary(mesh: &SkeletalMeshAsset, bone: &str) -> (usize, f32) {
    let Some(lod) = mesh.lod0() else {
        return (0, 0.0);
    };
    let weights = bone_influence(mesh, lod, bone);
    let influenced = weights.iter().filter(|w| **w > 0.0).count();
    (influenced, weights.iter().copied().fold(0.0, f32::max))
}

impl MeshInspectorPanel {
    pub fn new(editor: WeakEntity<SkeletalAnimEditorPanel>, cx: &mut Context<Self>) -> Self {
        Self {
            editor,
            focus_handle: cx.focus_handle(),
        }
    }

    fn section(&self, title: &'static str, icon: IconName, cx: &App) -> Div {
        let theme = cx.theme();
        v_flex()
            .w_full()
            .p_3()
            .gap_2()
            .border_b_1()
            .border_color(theme.border.opacity(0.6))
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(Icon::new(icon).size(px(13.0)).text_color(theme.info))
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .text_color(theme.foreground)
                            .child(title),
                    ),
            )
    }

    fn stat(&self, label: &'static str, value: String, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .w_full()
            .justify_between()
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(label),
            )
            .child(
                div()
                    .text_xs()
                    .font_family(MONO)
                    .text_color(theme.foreground)
                    .child(value),
            )
    }

    fn render_stats(&self, stats: MeshStats, issues: &[Issue], cx: &App) -> impl IntoElement {
        self.section("Statistics", IconName::Info, cx)
            .child(self.stat("Vertices", stats.vertices.to_string(), cx))
            .child(self.stat("Triangles", stats.triangles.to_string(), cx))
            .child(self.stat("Joints", stats.joints.to_string(), cx))
            .child(self.stat("Max influences", stats.max_influences.to_string(), cx))
            .child(self.stat("Sections", stats.sections.to_string(), cx))
            .child(self.stat("Material slots", stats.material_slots.to_string(), cx))
            .child(self.stat("LODs", stats.lods.to_string(), cx))
            .child(self.stat("Issues", issue_summary(issues), cx))
    }

    fn render_slots(
        &self,
        mesh: &SkeletalMeshAsset,
        pickers: &[Entity<MeshAssetPicker>],
        editor: &Entity<SkeletalAnimEditorPanel>,
        cx: &App,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let mut section = self.section("Material slots", IconName::ColorWheel, cx);
        if mesh.material_slots.is_empty() {
            section = section.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("This mesh has no material slots."),
            );
        }
        for (index, slot) in mesh.material_slots.iter().enumerate() {
            let label = match slot.material.as_deref() {
                Some(path) if !path.is_empty() => {
                    path.rsplit('/').next().unwrap_or(path).to_string()
                }
                _ => "None".to_string(),
            };
            let tooltip = slot
                .material
                .clone()
                .unwrap_or_else(|| "No material assigned".to_string());
            let trigger = Button::new(("slot-material", index))
                .label(label)
                .small()
                .outline()
                .dropdown_caret(true)
                .tooltip(tooltip);
            let field: AnyElement = match pickers.get(index) {
                Some(picker) => {
                    let picker = picker.clone();
                    Popover::<MeshAssetPicker>::new(("slot-popover", index))
                        .anchor(Corner::BottomRight)
                        .trigger(trigger)
                        .content(move |_, _| picker.clone())
                        .into_any_element()
                }
                None => trigger.into_any_element(),
            };
            let editor = editor.clone();
            section = section.child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .w(px(20.0))
                            .text_xs()
                            .font_family(MONO)
                            .text_color(theme.muted_foreground)
                            .child(index.to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(theme.foreground)
                            .child(slot.name.clone()),
                    )
                    .child(field)
                    .when(
                        slot.material.is_some() || slot.material_id.is_some(),
                        |row| {
                            row.child(
                                Button::new(("slot-clear", index))
                                    .icon(IconName::Close)
                                    .ghost()
                                    .xsmall()
                                    .tooltip("Clear the material")
                                    .on_click(move |_, _window, cx| {
                                        editor.update(cx, |e, cx| {
                                            e.set_slot_material(index, None, None, cx)
                                        });
                                    }),
                            )
                        },
                    ),
            );
        }
        section.child(
            div().text_xs().text_color(theme.muted_foreground).child(
                "Material assignments use stable IDs and follow materials when they move or are renamed.",
            ),
        )
    }

    fn render_joints(
        &self,
        mesh: &SkeletalMeshAsset,
        rows: Vec<JointRow>,
        selected: Option<&str>,
        editor: &Entity<SkeletalAnimEditorPanel>,
        cx: &App,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let missing = rows.iter().filter(|r| r.missing).count();
        let mut section = self.section("Joints", IconName::Component, cx);

        if missing > 0 {
            section = section.child(Tag::danger().small().child(format!(
                "{missing} joint(s) follow bones the skeleton lacks"
            )));
        }
        if let Some(bone) = selected {
            let (influenced, strongest) = bone_weight_summary(mesh, bone);
            section = section.child(
                v_flex()
                    .gap_0p5()
                    .p_2()
                    .rounded(px(6.0))
                    .bg(theme.info.opacity(0.10))
                    .border_1()
                    .border_color(theme.info.opacity(0.35))
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(theme.foreground)
                            .child(bone.to_string()),
                    )
                    .child(div().text_xs().text_color(theme.muted_foreground).child(
                        if influenced == 0 {
                            "No vertex follows this bone".to_string()
                        } else {
                            format!(
                                "Influences {influenced} vertices, strongest weight {strongest:.2}"
                            )
                        },
                    )),
            );
        }

        for row in rows {
            let is_selected = selected == Some(row.bone.as_str());
            let editor = editor.clone();
            let bone = row.bone.clone();
            let text = if row.missing {
                theme.danger
            } else {
                theme.foreground
            };
            section = section.child(
                h_flex()
                    .id(("joint", row.index))
                    .w_full()
                    .h(px(24.0))
                    .px_1p5()
                    .gap_2()
                    .items_center()
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .when(is_selected, |r| r.bg(theme.accent.opacity(0.35)))
                    .hover(|r| r.bg(theme.accent.opacity(0.18)))
                    .on_click(move |_, window, cx| {
                        let bone = bone.clone();
                        editor.update(cx, |e, cx| e.select_bone(Some(bone), window, cx));
                    })
                    .child(
                        div()
                            .w(px(24.0))
                            .text_xs()
                            .font_family(MONO)
                            .text_color(theme.muted_foreground)
                            .child(row.index.to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(text)
                            .child(row.name.clone()),
                    )
                    .when(row.missing, |r| {
                        r.child(Tag::danger().small().child("missing"))
                    })
                    .child(
                        div()
                            .text_xs()
                            .font_family(MONO)
                            .text_color(theme.muted_foreground)
                            .child(format!("{} v", row.vertices)),
                    ),
            );
        }
        section
    }
}

impl EventEmitter<PanelEvent> for MeshInspectorPanel {}

impl Focusable for MeshInspectorPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ui::dock::Panel for MeshInspectorPanel {
    fn panel_name(&self) -> &'static str {
        "skeletal-mesh-inspector"
    }

    fn title(&self, _window: &Window, _cx: &App) -> AnyElement {
        "Mesh".into_any_element()
    }
}

impl Render for MeshInspectorPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let header = panel_header(IconName::Cube, "Skeletal Mesh", None, cx);
        let Some(editor) = self.editor.upgrade() else {
            return v_flex()
                .size_full()
                .bg(theme.sidebar)
                .child(header)
                .into_any_element();
        };

        let body: AnyElement = {
            let e = editor.read(cx);
            match e.mesh.as_deref() {
                None => div()
                    .flex_1()
                    .min_h_0()
                    .child(empty_message(
                        IconName::Cube,
                        "No mesh",
                        "This file has no geometry to inspect",
                        cx,
                    ))
                    .into_any_element(),
                Some(mesh) => {
                    let skeleton_known = matches!(e.link, crate::core::SkeletonLink::Resolved(_));
                    let rows = joint_rows(mesh, &e.skeleton, skeleton_known);
                    v_flex()
                        .id("mesh-inspector-scroll")
                        .flex_1()
                        .min_h_0()
                        .scrollable(ScrollbarAxis::Vertical)
                        .child(self.render_stats(mesh.stats(), &e.issues, cx))
                        .child(self.render_slots(mesh, &e.slot_pickers, &editor, cx))
                        .child(self.render_joints(
                            mesh,
                            rows,
                            e.selected_bone.as_deref(),
                            &editor,
                            cx,
                        ))
                        .into_any_element()
                }
            }
        };

        v_flex()
            .size_full()
            .bg(theme.sidebar)
            .child(header)
            .child(body)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: it would bring in gpui's `test` attribute macro.
    use super::{bone_weight_summary, issue_summary, joint_rows};
    use crate::core::sample;
    use crate::core::skmesh::SkeletalMeshAsset;
    use crate::core::validate::{Issue, Severity};
    use crate::core::Skeleton;

    #[test]
    fn the_joint_table_counts_vertices_and_flags_bones_the_skeleton_lacks() {
        let (skeleton, _) = sample::sample_assets();
        let mut mesh = sample::sample_mesh(&skeleton);
        let data = skeleton.skeleton();

        let rows = joint_rows(&mesh, &data, true);
        assert_eq!(rows.len(), mesh.joints.len());
        assert!(rows.iter().all(|r| !r.missing));
        assert!(
            rows.iter().any(|r| r.vertices > 0),
            "some joint has vertices"
        );
        assert_eq!(rows[1].index, 1);
        assert_eq!(rows[1].bone, mesh.joints[1].bone);

        mesh.joints[2].bone = "tail".into();
        let rows = joint_rows(&mesh, &data, true);
        assert!(rows[2].missing);
        assert_eq!(rows[2].name, "tail", "an unknown bone is shown by its id");
        assert_eq!(rows.iter().filter(|r| r.missing).count(), 1);

        // Without a skeleton nothing can be called missing.
        assert!(joint_rows(&mesh, &Skeleton::default(), false)
            .iter()
            .all(|r| !r.missing));
    }

    #[test]
    fn the_issue_summary_counts_errors_and_warnings() {
        let issue = |severity| Issue::new(severity, "x");
        assert_eq!(issue_summary(&[]), "none");
        assert_eq!(issue_summary(&[issue(Severity::Info)]), "none");
        assert_eq!(
            issue_summary(&[issue(Severity::Error), issue(Severity::Error)]),
            "2 error(s)"
        );
        assert_eq!(
            issue_summary(&[issue(Severity::Error), issue(Severity::Warning)]),
            "1 error(s), 1 warning(s)"
        );
    }

    #[test]
    fn a_bones_weight_summary_counts_the_vertices_it_moves() {
        let (skeleton, _) = sample::sample_assets();
        let mesh = sample::sample_mesh(&skeleton);
        let bone = mesh.joints[1].bone.clone();
        let (influenced, strongest) = bone_weight_summary(&mesh, &bone);
        assert!(influenced > 0 && strongest > 0.0 && strongest <= 1.0);
        assert_eq!(bone_weight_summary(&mesh, "nope"), (0, 0.0));
        assert_eq!(
            bone_weight_summary(&SkeletalMeshAsset::new(None), "root"),
            (0, 0.0)
        );
    }
}
