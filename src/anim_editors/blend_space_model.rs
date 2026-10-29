//! Blend Space editor model: samples on a 1D strip or a 2D plot.
//!
//! The model owns the document, the selection and the (never saved) live
//! input. Weights and triangulation come from the runtime's own solver
//! ([`CompiledBlendSpace::layout`]), so the plot shows exactly what will play.

use std::sync::Arc;

use pulsar_reflection::{AnimationSequenceAssetKind, AssetRef};
use skeletal_assets::blend_space::{BlendAxis, BlendSample, BlendSpaceAsset, EdgePolicy, SampleLoop, SyncMode};
use skeletal_assets::diagnostics::Diagnostic;
use skeletal_assets::runtime::blendspace::CompiledBlendSpace;

use super::document::{AnimAsset, AnimDocument};
use super::selection::AnimSelection;
use super::store::DocStore;

/// Weights and triangulation for the current layout and input.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BlendPreview {
    /// `(sample id, normalised weight)` of the contributing samples.
    pub weights: Vec<(String, f32)>,
    /// Triangles as sample ids (2D only).
    pub triangles: Vec<[String; 3]>,
    /// The input after the axis edge policy.
    pub clamped_input: (f32, f32),
}

/// Maps axis values to plot pixels and back. Y grows upward in value space and
/// downward on screen.
#[derive(Clone, Copy, Debug)]
pub struct PlotTransform {
    pub width: f32,
    pub height: f32,
    pub pad: f32,
    pub x_min: f32,
    pub x_max: f32,
    pub y_min: f32,
    pub y_max: f32,
}

impl PlotTransform {
    pub fn for_asset(asset: &BlendSpaceAsset, width: f32, height: f32, pad: f32) -> Self {
        let (y_min, y_max) = asset.y_axis.as_ref().map_or((0.0, 1.0), |a| (a.min, a.max));
        Self { width, height, pad, x_min: asset.x_axis.min, x_max: asset.x_axis.max, y_min, y_max }
    }

    pub fn to_plot(&self, x: f32, y: f32) -> (f32, f32) {
        let fx = (x - self.x_min) / (self.x_max - self.x_min);
        let fy = (y - self.y_min) / (self.y_max - self.y_min);
        (self.pad + fx * (self.width - 2.0 * self.pad), self.height - self.pad - fy * (self.height - 2.0 * self.pad))
    }

    pub fn from_plot(&self, px: f32, py: f32) -> (f32, f32) {
        let fx = (px - self.pad) / (self.width - 2.0 * self.pad);
        let fy = (self.height - self.pad - py) / (self.height - 2.0 * self.pad);
        (self.x_min + fx * (self.x_max - self.x_min), self.y_min + fy * (self.y_max - self.y_min))
    }
}

pub struct BlendSpaceModel {
    pub doc: AnimDocument<BlendSpaceAsset>,
    pub selection: AnimSelection,
    /// Live preview input. Editor state only.
    pub input: (f32, f32),
    pub snap_to_grid: bool,
    preview_cache: Option<(u64, (f32, f32), BlendPreview)>,
}

impl BlendSpaceModel {
    pub fn open(store: Arc<dyn DocStore>, path: &std::path::Path, default_text: &str) -> Result<Self, String> {
        let doc = AnimDocument::<BlendSpaceAsset>::open(store, path, default_text)?;
        let input = (doc.value().x_axis.min, doc.value().y_axis.as_ref().map_or(0.0, |a| a.min));
        Ok(Self { doc, selection: AnimSelection::None, input, snap_to_grid: true, preview_cache: None })
    }

    pub fn asset(&self) -> &BlendSpaceAsset {
        self.doc.value()
    }

    fn sample_index(&self, id: &str) -> Option<usize> {
        self.asset().samples.iter().position(|s| s.id == id)
    }

    pub fn selected_sample(&self) -> Option<&BlendSample> {
        match &self.selection {
            AnimSelection::BlendSample { id } => self.asset().samples.iter().find(|s| &s.id == id),
            _ => None,
        }
    }

    pub fn select_sample(&mut self, id: &str) -> bool {
        if self.sample_index(id).is_some() {
            self.selection = AnimSelection::BlendSample { id: id.to_string() };
            true
        } else {
            false
        }
    }

    pub fn select_axis(&mut self, vertical: bool) {
        if !vertical || self.asset().is_2d() {
            self.selection = AnimSelection::BlendAxis { vertical };
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection = AnimSelection::None;
    }

    fn snapped(&self, value: f32, axis: &BlendAxis) -> f32 {
        let value = value.clamp(axis.min, axis.max);
        if !self.snap_to_grid {
            return value;
        }
        let step = axis.span() / axis.grid_divisions.max(1) as f32;
        (axis.min + ((value - axis.min) / step).round() * step).clamp(axis.min, axis.max)
    }

    /// Add a sample at `(x, y)` (axis values), snapped and clamped. A position
    /// already occupied is nudged one grid step so no two samples coincide.
    pub fn add_sample(&mut self, x: f32, y: f32, sequence: Option<AssetRef<AnimationSequenceAssetKind>>) -> String {
        let (mut sx, mut sy) = (self.snapped(x, &self.asset().x_axis), self.asset().y_axis.as_ref().map(|a| self.snapped(y, a)));
        let two_d = self.asset().is_2d();
        let step_x = self.asset().x_axis.span() / self.asset().x_axis.grid_divisions.max(1) as f32;
        let taken = |s: &BlendSpaceAsset, x: f32, y: Option<f32>| s.samples.iter().any(|o| o.x == x && o.y == y);
        let mut guard = 0;
        while taken(self.asset(), sx, sy) && guard < 1000 {
            sx += step_x;
            if sx > self.asset().x_axis.max {
                sx = self.asset().x_axis.min;
                if let (Some(axis), Some(y)) = (self.asset().y_axis.as_ref(), sy) {
                    sy = Some((y + axis.span() / axis.grid_divisions.max(1) as f32).min(axis.max));
                }
            }
            guard += 1;
        }
        let sample = BlendSample::new(sequence, sx, if two_d { sy } else { None });
        let id = sample.id.clone();
        self.doc.apply("Add sample", |a| a.samples.push(sample));
        self.selection = AnimSelection::BlendSample { id: id.clone() };
        id
    }

    /// Drag a sample. Consecutive moves of one sample are one undo step until
    /// [`end_gesture`](Self::end_gesture).
    pub fn move_sample(&mut self, id: &str, x: f32, y: f32) -> bool {
        let Some(index) = self.sample_index(id) else { return false };
        let nx = self.snapped(x, &self.asset().x_axis);
        let ny = self.asset().y_axis.as_ref().map(|a| self.snapped(y, a));
        // refuse a move onto another sample, which would make weights ambiguous
        if self.asset().samples.iter().enumerate().any(|(i, o)| i != index && o.x == nx && o.y == ny) {
            return false;
        }
        let key = format!("move:{id}");
        self.doc.edit("Move sample", Some(&key), |a| {
            let s = &mut a.samples[index];
            let changed = s.x != nx || s.y != ny;
            s.x = nx;
            s.y = ny;
            (changed, changed)
        })
    }

    pub fn end_gesture(&mut self) {
        self.doc.end_gesture();
    }

    /// Set a sample's coordinate exactly (typed in the inspector), without snapping.
    pub fn set_sample_position(&mut self, id: &str, x: Option<f32>, y: Option<f32>) -> Result<(), String> {
        let index = self.sample_index(id).ok_or("no such sample")?;
        let asset = self.asset();
        let (nx, ny) = (x.unwrap_or(asset.samples[index].x), y.or(asset.samples[index].y));
        if !nx.is_finite() || ny.is_some_and(|v| !v.is_finite()) {
            return Err("position must be a number".into());
        }
        if asset.samples.iter().enumerate().any(|(i, o)| i != index && o.x == nx && o.y == ny) {
            return Err("another sample is already at that position".into());
        }
        let two_d = asset.is_2d();
        let key = format!("pos:{id}");
        self.doc.edit("Set sample position", Some(&key), |a| {
            let s = &mut a.samples[index];
            s.x = nx;
            s.y = if two_d { ny } else { None };
            (true, ())
        });
        Ok(())
    }

    pub fn set_sample_sequence(&mut self, id: &str, sequence: Option<AssetRef<AnimationSequenceAssetKind>>) -> bool {
        let Some(index) = self.sample_index(id) else { return false };
        self.doc.edit("Set sample sequence", None, |a| {
            let changed = a.samples[index].sequence != sequence;
            a.samples[index].sequence = sequence;
            (changed, changed)
        })
    }

    pub fn set_sample_rate(&mut self, id: &str, rate: f32) -> Result<(), String> {
        if !rate.is_finite() || rate <= 0.0 {
            return Err("play rate must be positive".into());
        }
        let index = self.sample_index(id).ok_or("no such sample")?;
        let key = format!("rate:{id}");
        self.doc.edit("Set play rate", Some(&key), |a| {
            a.samples[index].play_rate = rate;
            (true, ())
        });
        Ok(())
    }

    pub fn set_sample_phase(&mut self, id: &str, phase: f32) -> Result<(), String> {
        if !phase.is_finite() || !(0.0..1.0).contains(&phase) {
            return Err("phase offset must be in [0, 1)".into());
        }
        let index = self.sample_index(id).ok_or("no such sample")?;
        let key = format!("phase:{id}");
        self.doc.edit("Set phase offset", Some(&key), |a| {
            a.samples[index].phase_offset = phase;
            (true, ())
        });
        Ok(())
    }

    pub fn set_sample_loop(&mut self, id: &str, looping: SampleLoop) -> bool {
        let Some(index) = self.sample_index(id) else { return false };
        self.doc.edit("Set sample looping", None, |a| {
            let changed = a.samples[index].looping != looping;
            a.samples[index].looping = looping;
            (changed, changed)
        })
    }

    pub fn set_sample_sync_group(&mut self, id: &str, group: Option<String>) -> bool {
        let Some(index) = self.sample_index(id) else { return false };
        let group = group.filter(|g| !g.trim().is_empty());
        self.doc.edit("Set sync group", None, |a| {
            let changed = a.samples[index].sync_group != group;
            a.samples[index].sync_group = group;
            (changed, changed)
        })
    }

    pub fn delete_sample(&mut self, id: &str) -> bool {
        let Some(index) = self.sample_index(id) else { return false };
        self.doc.apply("Delete sample", |a| {
            a.samples.remove(index);
            if a.sync.leader.as_deref() == Some(id) {
                a.sync.leader = None;
            }
        });
        if matches!(&self.selection, AnimSelection::BlendSample { id: s } if s == id) {
            self.selection = AnimSelection::None;
        }
        true
    }

    /// Edit an axis. Rejected edits leave the document untouched.
    pub fn set_axis(&mut self, vertical: bool, edit: impl FnOnce(&mut BlendAxis)) -> Result<(), String> {
        if vertical && !self.asset().is_2d() {
            return Err("this is a 1D blend space".into());
        }
        let mut candidate = self.asset().clone();
        {
            let axis = if vertical { candidate.y_axis.as_mut() } else { Some(&mut candidate.x_axis) };
            edit(axis.ok_or("no vertical axis")?);
        }
        candidate.validate_structure().or_else(|e| {
            // Samples left outside a narrowed axis are a diagnostic, not a refusal.
            if e.contains("outside") { Ok(()) } else { Err(e) }
        })?;
        let key = format!("axis:{vertical}");
        self.doc.edit("Edit axis", Some(&key), |a| {
            a.x_axis = candidate.x_axis;
            a.y_axis = candidate.y_axis;
            (true, ())
        });
        Ok(())
    }

    pub fn set_edge_policy(&mut self, vertical: bool, edge: EdgePolicy) -> Result<(), String> {
        self.set_axis(vertical, |a| a.edge = edge)
    }

    /// Switch between a 1D strip and a 2D plot. Going 2D gives every sample the
    /// vertical minimum; going 1D drops the vertical coordinate and any sample
    /// that would now coincide with another is reported by validation.
    pub fn set_two_dimensional(&mut self, two_d: bool) -> bool {
        if self.asset().is_2d() == two_d {
            return false;
        }
        self.doc.apply(if two_d { "Make 2D" } else { "Make 1D" }, |a| {
            if two_d {
                let y = BlendAxis { name: "Direction".into(), min: -180.0, max: 180.0, ..BlendAxis::default() };
                let min = y.min;
                a.y_axis = Some(y);
                for (i, s) in a.samples.iter_mut().enumerate() {
                    // spread new coordinates so the samples do not coincide
                    s.y = Some(min + (i as f32) * 360.0 / a.x_axis.grid_divisions.max(1) as f32 % 360.0);
                }
            } else {
                a.y_axis = None;
                for s in &mut a.samples {
                    s.y = None;
                }
            }
        });
        if !two_d && matches!(self.selection, AnimSelection::BlendAxis { vertical: true }) {
            self.selection = AnimSelection::None;
        }
        true
    }

    pub fn set_sync_mode(&mut self, mode: SyncMode) -> bool {
        self.doc.edit("Set sync mode", None, |a| {
            let changed = a.sync.mode != mode;
            a.sync.mode = mode;
            (changed, changed)
        })
    }

    pub fn set_sync_leader(&mut self, leader: Option<String>) -> Result<(), String> {
        if let Some(l) = &leader {
            if self.sample_index(l).is_none() {
                return Err("the leader must be one of the samples".into());
            }
        }
        self.doc.edit("Set sync leader", None, |a| {
            let changed = a.sync.leader != leader;
            a.sync.leader = leader;
            (changed, ())
        });
        Ok(())
    }

    pub fn rename(&mut self, name: &str) -> bool {
        let name = name.trim().to_string();
        if name.is_empty() {
            return false;
        }
        self.doc.edit("Rename", Some("rename"), |a| {
            let changed = a.name != name;
            a.set_asset_name(name);
            (changed, changed)
        })
    }

    pub fn set_input(&mut self, x: f32, y: f32) {
        self.input = (x, y);
    }

    /// Weights and triangulation for the current layout and input.
    pub fn preview(&mut self) -> BlendPreview {
        let key = (self.doc.generation(), self.input);
        if let Some((generation, input, preview)) = &self.preview_cache {
            if (*generation, *input) == key {
                return preview.clone();
            }
        }
        let preview = self.compute_preview();
        self.preview_cache = Some((key.0, key.1, preview.clone()));
        preview
    }

    fn compute_preview(&self) -> BlendPreview {
        let asset = self.asset();
        let Some(space) = CompiledBlendSpace::layout(asset) else { return BlendPreview::default() };
        let id_of = |i: usize| asset.samples.get(i).map(|s| s.id.clone()).unwrap_or_default();
        BlendPreview {
            weights: space.solve(self.input.0, self.input.1).into_iter().map(|w| (id_of(w.sample), w.weight)).collect(),
            triangles: space.triangles().iter().map(|t| [id_of(t[0]), id_of(t[1]), id_of(t[2])]).collect(),
            clamped_input: space.clamp_input(self.input.0, self.input.1),
        }
    }

    /// Sample under a plot position, nearest first, within `radius` pixels.
    pub fn hit_sample(&self, transform: &PlotTransform, px: f32, py: f32, radius: f32) -> Option<String> {
        self.asset()
            .samples
            .iter()
            .map(|s| {
                let (sx, sy) = transform.to_plot(s.x, s.y.unwrap_or(0.5 * (transform.y_min + transform.y_max)));
                (((sx - px).powi(2) + (sy - py).powi(2)).sqrt(), s)
            })
            .filter(|(d, _)| *d <= radius)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, s)| s.id.clone())
    }

    /// Everything wrong with the document: structure first, then authoring.
    pub fn validation(&self) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        if let Err(message) = self.asset().validate_structure() {
            out.push(Diagnostic::error("blendspace.invalid", message));
        }
        out.extend(self.asset().diagnostics());
        out
    }

    pub fn save(&mut self) -> anyhow::Result<super::document::SaveOutcome> {
        self.doc.save()
    }
}

#[cfg(test)]
mod tests {
    use super::super::store::MemStore;
    use super::*;
    use skeletal_assets::issue::Severity;

    fn model() -> (BlendSpaceModel, MemStore) {
        let store = MemStore::default();
        let m = BlendSpaceModel::open(Arc::new(store.clone()), std::path::Path::new("l.blendspace"), &BlendSpaceAsset::default().to_text().unwrap()).unwrap();
        (m, store)
    }

    fn seq() -> Option<AssetRef<AnimationSequenceAssetKind>> {
        Some(AssetRef::new())
    }

    #[test]
    fn samples_are_added_snapped_selected_and_nudged_off_each_other() {
        let (mut m, _) = model();
        // axis 0..1, 4 divisions
        let a = m.add_sample(0.31, 0.0, seq());
        assert_eq!(m.asset().samples[0].x, 0.25, "snapped to the grid");
        assert_eq!(m.selection, AnimSelection::BlendSample { id: a.clone() });
        let b = m.add_sample(0.26, 0.0, seq());
        assert_ne!(m.asset().samples.iter().find(|s| s.id == b).unwrap().x, 0.25, "never on top of another sample");
        m.snap_to_grid = false;
        let c = m.add_sample(5.0, 0.0, seq());
        assert_eq!(m.asset().samples.iter().find(|s| s.id == c).unwrap().x, 1.0, "clamped to the axis");
    }

    #[test]
    fn dragging_is_one_undo_step_and_refuses_collisions() {
        let (mut m, _) = model();
        m.snap_to_grid = false;
        let a = m.add_sample(0.0, 0.0, seq());
        let _b = m.add_sample(1.0, 0.0, seq());
        for x in [0.1, 0.2, 0.3, 0.4] {
            assert!(m.move_sample(&a, x, 0.0));
        }
        m.end_gesture();
        assert_eq!(m.asset().samples[0].x, 0.4);
        assert!(!m.move_sample(&a, 1.0, 0.0), "cannot land on another sample");
        assert!(m.doc.undo());
        assert_eq!(m.asset().samples[0].x, 0.0, "the whole drag undone at once");
    }

    #[test]
    fn two_d_weights_triangles_and_hit_testing_come_from_the_runtime_solver() {
        let (mut m, _) = model();
        m.set_two_dimensional(true);
        m.snap_to_grid = false;
        let ids: Vec<_> = [(0.0, -180.0), (1.0, -180.0), (0.5, 180.0)].into_iter().map(|(x, y)| m.add_sample(x, y, seq())).collect();
        m.set_input(0.5, 0.0);
        let p = m.preview();
        assert_eq!(p.triangles.len(), 1);
        let total: f32 = p.weights.iter().map(|(_, w)| w).sum();
        assert!((total - 1.0).abs() < 1e-4);
        assert_eq!(p.weights.len(), 3, "inside the triangle all three contribute");
        m.set_input(0.0, -180.0);
        assert_eq!(m.preview().weights, vec![(ids[0].clone(), 1.0)], "on a vertex only that sample contributes");

        let t = PlotTransform::for_asset(m.asset(), 400.0, 300.0, 20.0);
        let (px, py) = t.to_plot(1.0, -180.0);
        assert_eq!(m.hit_sample(&t, px + 3.0, py - 2.0, 8.0), Some(ids[1].clone()));
        assert_eq!(m.hit_sample(&t, px + 60.0, py, 8.0), None);
        let (x, y) = t.from_plot(px, py);
        assert!((x - 1.0).abs() < 1e-4 && (y + 180.0).abs() < 1e-3);
    }

    #[test]
    fn one_d_strip_blends_between_neighbours() {
        let (mut m, _) = model();
        m.snap_to_grid = false;
        let a = m.add_sample(0.0, 0.0, seq());
        let b = m.add_sample(1.0, 0.0, seq());
        m.set_input(0.25, 0.0);
        let w = m.preview().weights;
        assert_eq!(w.len(), 2);
        let wa = w.iter().find(|(i, _)| *i == a).unwrap().1;
        let wb = w.iter().find(|(i, _)| *i == b).unwrap().1;
        assert!((wa - 0.75).abs() < 1e-4 && (wb - 0.25).abs() < 1e-4);
    }

    #[test]
    fn axis_edits_are_validated_and_dimension_switch_round_trips() {
        let (mut m, _) = model();
        assert!(m.set_axis(false, |a| a.max = a.min).is_err(), "min must stay below max");
        assert!(m.set_axis(true, |_| {}).is_err(), "no vertical axis in 1D");
        m.set_axis(false, |a| {
            a.name = "Speed".into();
            a.max = 600.0;
        })
        .unwrap();
        assert_eq!(m.asset().x_axis.max, 600.0);
        m.add_sample(0.0, 0.0, seq());
        assert!(m.set_two_dimensional(true) && m.asset().samples[0].y.is_some());
        assert!(m.asset().validate_structure().is_ok());
        assert!(m.set_two_dimensional(false) && m.asset().samples[0].y.is_none());
        assert!(!m.set_two_dimensional(false));
    }

    #[test]
    fn sample_properties_are_validated() {
        let (mut m, _) = model();
        let a = m.add_sample(0.0, 0.0, seq());
        assert!(m.set_sample_rate(&a, 0.0).is_err());
        assert!(m.set_sample_rate(&a, 1.5).is_ok());
        assert!(m.set_sample_phase(&a, 1.0).is_err());
        assert!(m.set_sample_phase(&a, 0.25).is_ok());
        assert!(m.set_sample_loop(&a, SampleLoop::Loop));
        assert!(m.set_sample_sync_group(&a, Some("  ".into())) == false, "blank group is no group");
        assert!(m.set_sample_sync_group(&a, Some("walk".into())));
        assert!(m.set_sync_leader(Some("ghost".into())).is_err());
        m.set_sync_leader(Some(a.clone())).unwrap();
        m.delete_sample(&a);
        assert_eq!(m.asset().sync.leader, None, "deleting the leader clears it");
        assert!(m.selection.is_none());
    }

    #[test]
    fn validation_reports_empty_samples_with_a_repair_and_clears_when_fixed() {
        let (mut m, _) = model();
        let a = m.add_sample(0.0, 0.0, None);
        let v = m.validation();
        let empty = v.iter().find(|d| d.code == "blendspace.empty_sample").expect("empty sample reported");
        assert_eq!(empty.severity, Severity::Error);
        assert_eq!(empty.location.as_deref(), Some(a.as_str()));
        assert!(empty.repair.is_some());
        m.set_sample_sequence(&a, seq());
        assert!(!m.validation().iter().any(|d| d.code == "blendspace.empty_sample"));
    }

    #[test]
    fn every_authored_field_round_trips_through_save_and_reopen() {
        let (mut m, store) = model();
        m.set_two_dimensional(true);
        m.snap_to_grid = false;
        let s = seq();
        let a = m.add_sample(0.4, 10.0, s.clone());
        m.set_sample_rate(&a, 1.25).unwrap();
        m.set_sample_phase(&a, 0.5).unwrap();
        m.set_sample_loop(&a, SampleLoop::Once);
        m.set_sample_sync_group(&a, Some("feet".into()));
        m.set_sync_mode(SyncMode::Markers);
        m.set_sync_leader(Some(a.clone())).unwrap();
        m.set_edge_policy(true, EdgePolicy::Wrap).unwrap();
        m.rename("Locomotion 2D");
        assert!(m.doc.is_dirty());
        assert_eq!(m.save().unwrap(), super::super::document::SaveOutcome::Saved);
        let reopened = BlendSpaceModel::open(Arc::new(store), std::path::Path::new("l.blendspace"), "").unwrap();
        let r = reopened.asset();
        let sample = &r.samples[0];
        assert_eq!((sample.x, sample.y, sample.play_rate, sample.phase_offset), (0.4, Some(10.0), 1.25, 0.5));
        assert_eq!((sample.looping, sample.sync_group.as_deref(), sample.sequence.clone()), (SampleLoop::Once, Some("feet"), s));
        assert_eq!((r.sync.mode, r.sync.leader.clone()), (SyncMode::Markers, Some(a)));
        assert_eq!((r.y_axis.as_ref().unwrap().edge, r.name.as_str()), (EdgePolicy::Wrap, "Locomotion 2D"));
    }

    #[test]
    fn selection_is_never_saved() {
        let (mut m, store) = model();
        let a = m.add_sample(0.0, 0.0, seq());
        m.select_sample(&a);
        m.set_input(0.7, 0.0);
        m.save().unwrap();
        let text = store.text("l.blendspace").unwrap();
        assert!(!text.contains("selection") && !text.contains("0.7"));
    }
}
