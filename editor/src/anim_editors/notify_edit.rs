//! Notify and notify-state editing shared by the Sequence and Montage editors,
//! plus the lane layout that keeps dense markers readable.
//!
//! Both asset kinds store the same three lists (instant notifies, ranged
//! states, payload schemas). [`NotifyView`] borrows them and exposes every
//! authoring command; each model wraps a call in `AnimDocument::edit` so the
//! change is undoable. All times are seconds on the asset's own timeline and
//! are clamped to `[0, limit]`, where `limit` is the clip or montage length.

use skeletal_assets::diagnostics::Diagnostic;
use skeletal_assets::notify_schema::{check_payload, AnimationNotifyState, NotifyFieldDef, NotifyFieldType, NotifyPayloadSchema};
use skeletal_assets::notifies::{AnimationNotify, NotifyField, NotifyTarget, NotifyValue};

/// Smallest range a state may be dragged or resized to.
pub const MIN_STATE_LENGTH: f32 = 0.01;

pub struct NotifyView<'a> {
    pub notifies: &'a mut Vec<AnimationNotify>,
    pub states: &'a mut Vec<AnimationNotifyState>,
    pub schemas: &'a mut Vec<NotifyPayloadSchema>,
}

fn identifier_ok(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Which list an id lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotifyKind {
    Notify,
    State,
}

impl NotifyView<'_> {
    pub fn kind_of(&self, id: &str) -> Option<NotifyKind> {
        if self.notifies.iter().any(|n| n.id == id) {
            Some(NotifyKind::Notify)
        } else if self.states.iter().any(|s| s.id == id) {
            Some(NotifyKind::State)
        } else {
            None
        }
    }

    pub fn add_notify(&mut self, name: &str, time: f32, limit: f32) -> String {
        let mut notify = AnimationNotify::new(name.trim().to_string(), time.clamp(0.0, limit.max(0.0)));
        if notify.name.is_empty() {
            notify.name = "Notify".into();
        }
        let id = notify.id.clone();
        self.notifies.push(notify);
        id
    }

    pub fn add_state(&mut self, name: &str, start: f32, end: f32, limit: f32) -> String {
        let limit = limit.max(MIN_STATE_LENGTH);
        let start = start.clamp(0.0, limit - MIN_STATE_LENGTH);
        let end = end.clamp(start + MIN_STATE_LENGTH, limit);
        let mut state = AnimationNotifyState::new(name.trim().to_string(), start, end);
        if state.name.is_empty() {
            state.name = "State".into();
        }
        let id = state.id.clone();
        self.states.push(state);
        id
    }

    /// Move a notify, or slide a state keeping its length. Returns whether anything changed.
    pub fn move_to(&mut self, id: &str, time: f32, limit: f32) -> bool {
        if let Some(n) = self.notifies.iter_mut().find(|n| n.id == id) {
            let t = time.clamp(0.0, limit.max(0.0));
            let changed = n.time != t;
            n.time = t;
            return changed;
        }
        if let Some(s) = self.states.iter_mut().find(|s| s.id == id) {
            let length = s.end - s.start;
            let start = time.clamp(0.0, (limit - length).max(0.0));
            let changed = s.start != start;
            s.start = start;
            s.end = start + length;
            return changed;
        }
        false
    }

    /// Set one or both ends of a state, keeping at least [`MIN_STATE_LENGTH`].
    pub fn resize_state(&mut self, id: &str, start: Option<f32>, end: Option<f32>, limit: f32) -> bool {
        let Some(s) = self.states.iter_mut().find(|s| s.id == id) else { return false };
        let new_start = start.unwrap_or(s.start).clamp(0.0, limit.max(MIN_STATE_LENGTH) - MIN_STATE_LENGTH);
        let new_end = end.unwrap_or(s.end).clamp(new_start + MIN_STATE_LENGTH, limit.max(new_start + MIN_STATE_LENGTH));
        let new_start = new_start.min(new_end - MIN_STATE_LENGTH);
        let changed = (s.start, s.end) != (new_start, new_end);
        s.start = new_start;
        s.end = new_end;
        changed
    }

    pub fn delete(&mut self, id: &str) -> bool {
        let before = (self.notifies.len(), self.states.len());
        self.notifies.retain(|n| n.id != id);
        self.states.retain(|s| s.id != id);
        before != (self.notifies.len(), self.states.len())
    }

    pub fn rename(&mut self, id: &str, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        if let Some(n) = self.notifies.iter_mut().find(|n| n.id == id) {
            let changed = n.name != name;
            n.name = name.to_string();
            return changed;
        }
        if let Some(s) = self.states.iter_mut().find(|s| s.id == id) {
            let changed = s.name != name;
            s.name = name.to_string();
            return changed;
        }
        false
    }

    pub fn set_disabled(&mut self, id: &str, disabled: bool) -> bool {
        match self.states.iter_mut().find(|s| s.id == id) {
            Some(s) => {
                let changed = s.disabled != disabled;
                s.disabled = disabled;
                changed
            }
            None => false,
        }
    }

    pub fn set_tick_interval(&mut self, id: &str, interval: Option<f32>) -> Result<bool, String> {
        if interval.is_some_and(|v| !v.is_finite() || v <= 0.0) {
            return Err("tick interval must be positive".into());
        }
        let s = self.states.iter_mut().find(|s| s.id == id).ok_or("no such notify state")?;
        let changed = s.tick_interval != interval;
        s.tick_interval = interval;
        Ok(changed)
    }

    pub fn set_target(&mut self, id: &str, target: NotifyTarget) -> Result<bool, String> {
        if let NotifyTarget::ClassVariable(v) = &target {
            if !identifier_ok(v) {
                return Err("the target must name a class variable (letters, digits, underscore)".into());
            }
        }
        if let Some(n) = self.notifies.iter_mut().find(|n| n.id == id) {
            let changed = n.target != target;
            n.target = target;
            return Ok(changed);
        }
        if let Some(s) = self.states.iter_mut().find(|s| s.id == id) {
            let changed = s.target != target;
            s.target = target;
            return Ok(changed);
        }
        Err("no such notify".into())
    }

    fn fields_mut(&mut self, id: &str) -> Option<&mut Vec<NotifyField>> {
        if let Some(n) = self.notifies.iter_mut().find(|n| n.id == id) {
            return Some(&mut n.fields);
        }
        self.states.iter_mut().find(|s| s.id == id).map(|s| &mut s.fields)
    }

    pub fn fields(&self, id: &str) -> Option<&[NotifyField]> {
        if let Some(n) = self.notifies.iter().find(|n| n.id == id) {
            return Some(&n.fields);
        }
        self.states.iter().find(|s| s.id == id).map(|s| s.fields.as_slice())
    }

    pub fn schema_of(&self, id: &str) -> Option<&NotifyPayloadSchema> {
        let schema = self.notifies.iter().find(|n| n.id == id).map(|n| &n.schema).or_else(|| self.states.iter().find(|s| s.id == id).map(|s| &s.schema))?;
        schema.as_ref().and_then(|sid| self.schemas.iter().find(|s| &s.id == sid))
    }

    /// Set (or add) a payload field. With a schema, the name must be declared
    /// and the value is coerced to its type.
    pub fn set_field(&mut self, id: &str, name: &str, value: NotifyValue) -> Result<bool, String> {
        if !identifier_ok(name) {
            return Err("a field name is letters, digits and underscore, not starting with a digit".into());
        }
        if matches!(value, NotifyValue::Float(v) if !v.is_finite()) {
            return Err("numbers must be finite".into());
        }
        let value = match self.schema_of(id) {
            Some(schema) => {
                let def = schema.fields.iter().find(|d| d.name == name).ok_or_else(|| format!("`{name}` is not declared by schema `{}`", schema.name))?;
                coerce(&value, def.ty).ok_or_else(|| format!("`{name}` must be {:?}", def.ty))?
            }
            None => value,
        };
        let fields = self.fields_mut(id).ok_or("no such notify")?;
        match fields.iter_mut().find(|f| f.name == name) {
            Some(f) => {
                let changed = f.value != value;
                f.value = value;
                Ok(changed)
            }
            None => {
                fields.push(NotifyField { name: name.to_string(), value });
                Ok(true)
            }
        }
    }

    pub fn remove_field(&mut self, id: &str, name: &str) -> bool {
        match self.fields_mut(id) {
            Some(fields) => {
                let before = fields.len();
                fields.retain(|f| f.name != name);
                before != fields.len()
            }
            None => false,
        }
    }

    /// Bind a payload schema (or none). Declared fields missing from the
    /// payload are filled from the schema defaults, in schema order.
    pub fn set_schema(&mut self, id: &str, schema: Option<&str>) -> Result<bool, String> {
        let def = match schema {
            Some(sid) => Some(self.schemas.iter().find(|s| s.id == sid).cloned().ok_or("no such payload schema")?),
            None => None,
        };
        let target: (&mut Option<String>, &mut Vec<NotifyField>) = if let Some(n) = self.notifies.iter_mut().find(|n| n.id == id) {
            (&mut n.schema, &mut n.fields)
        } else if let Some(s) = self.states.iter_mut().find(|s| s.id == id) {
            (&mut s.schema, &mut s.fields)
        } else {
            return Err("no such notify".into());
        };
        let new = schema.map(str::to_string);
        let mut changed = *target.0 != new;
        *target.0 = new;
        if let Some(def) = def {
            for field in &def.fields {
                if !target.1.iter().any(|f| f.name == field.name) {
                    if let Some(value) = field.default.clone() {
                        target.1.push(NotifyField { name: field.name.clone(), value });
                        changed = true;
                    }
                }
            }
        }
        Ok(changed)
    }

    /// Drop payload fields the bound schema does not declare (the repair for
    /// the "undeclared field" diagnostic).
    pub fn remove_undeclared_fields(&mut self, id: &str) -> bool {
        let Some(schema) = self.schema_of(id).cloned() else { return false };
        match self.fields_mut(id) {
            Some(fields) => {
                let before = fields.len();
                fields.retain(|f| schema.fields.iter().any(|d| d.name == f.name));
                before != fields.len()
            }
            None => false,
        }
    }

    // ---- payload schemas ----------------------------------------------

    pub fn add_schema(&mut self, name: &str) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("a schema needs a name".into());
        }
        if self.schemas.iter().any(|s| s.name == name) {
            return Err("a schema with that name exists".into());
        }
        let schema = NotifyPayloadSchema::new(name);
        let id = schema.id.clone();
        self.schemas.push(schema);
        Ok(id)
    }

    pub fn schema_in_use(&self, schema_id: &str) -> bool {
        self.notifies.iter().any(|n| n.schema.as_deref() == Some(schema_id)) || self.states.iter().any(|s| s.schema.as_deref() == Some(schema_id))
    }

    pub fn delete_schema(&mut self, schema_id: &str) -> Result<bool, String> {
        if self.schema_in_use(schema_id) {
            return Err("the schema is used by a notify; unbind it first".into());
        }
        let before = self.schemas.len();
        self.schemas.retain(|s| s.id != schema_id);
        Ok(before != self.schemas.len())
    }

    pub fn add_schema_field(&mut self, schema_id: &str, name: &str, ty: NotifyFieldType, required: bool) -> Result<bool, String> {
        let schema = self.schemas.iter_mut().find(|s| s.id == schema_id).ok_or("no such payload schema")?;
        let mut candidate = schema.clone();
        candidate.fields.push(NotifyFieldDef { name: name.to_string(), ty, default: None, required });
        candidate.validate()?;
        *schema = candidate;
        Ok(true)
    }

    pub fn set_schema_default(&mut self, schema_id: &str, field: &str, default: Option<NotifyValue>) -> Result<bool, String> {
        let schema = self.schemas.iter_mut().find(|s| s.id == schema_id).ok_or("no such payload schema")?;
        let mut candidate = schema.clone();
        let def = candidate.fields.iter_mut().find(|f| f.name == field).ok_or("no such field")?;
        def.default = default;
        candidate.validate()?;
        let changed = *schema != candidate;
        *schema = candidate;
        Ok(changed)
    }

    pub fn remove_schema_field(&mut self, schema_id: &str, field: &str) -> Result<bool, String> {
        let schema = self.schemas.iter_mut().find(|s| s.id == schema_id).ok_or("no such payload schema")?;
        let before = schema.fields.len();
        schema.fields.retain(|f| f.name != field);
        Ok(before != schema.fields.len())
    }

    // ---- validation -----------------------------------------------------

    /// Problems with the records, each located at the notify id and with a repair
    /// when there is a mechanical one.
    pub fn diagnostics(&self, limit: f32) -> Vec<Diagnostic> {
        let mut out = Vec::new();
        for n in self.notifies.iter() {
            if n.time > limit + 1e-4 {
                out.push(Diagnostic::error("notify.outside", format!("Notify `{}` is after the end ({limit:.3} s)", n.name)).at(&n.id).repair("Move it inside the timeline"));
            }
            if let Err(message) = check_payload(self.schemas, &n.schema, &n.fields) {
                out.push(payload_diagnostic(&n.id, message));
            }
        }
        for s in self.states.iter() {
            if s.end > limit + 1e-4 {
                out.push(Diagnostic::error("notify.state_outside", format!("State `{}` ends after the end ({limit:.3} s)", s.name)).at(&s.id).repair("Trim the state"));
            }
            if let Err(message) = check_payload(self.schemas, &s.schema, &s.fields) {
                out.push(payload_diagnostic(&s.id, message));
            }
        }
        out
    }
}

fn payload_diagnostic(id: &str, message: String) -> Diagnostic {
    let repair = if message.contains("not declared") {
        "Remove the undeclared fields"
    } else if message.contains("Required field") {
        "Set the required field"
    } else {
        "Change the value to the declared type"
    };
    Diagnostic::error("notify.payload", message).at(id).repair(repair)
}

fn coerce(value: &NotifyValue, ty: NotifyFieldType) -> Option<NotifyValue> {
    Some(match (value, ty) {
        (NotifyValue::Bool(_), NotifyFieldType::Bool)
        | (NotifyValue::Int(_), NotifyFieldType::Int)
        | (NotifyValue::Float(_), NotifyFieldType::Float)
        | (NotifyValue::String(_), NotifyFieldType::String) => value.clone(),
        (NotifyValue::Int(i), NotifyFieldType::Float) => NotifyValue::Float(*i as f64),
        _ => return None,
    })
}

// ---- lane layout ---------------------------------------------------------

/// Layout input for one marker.
#[derive(Clone, Debug, PartialEq)]
pub struct LaneEntry {
    pub id: String,
    pub kind: NotifyKind,
    pub start: f32,
    /// Seconds; equal to `start` for an instant notify.
    pub end: f32,
    pub label_chars: usize,
}

/// A marker with its stack row and horizontal extent in pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct LanePlacement {
    pub id: String,
    pub kind: NotifyKind,
    pub row: usize,
    /// Left edge and right edge of everything the marker occupies, label included.
    pub x0: f32,
    pub x1: f32,
    /// Anchor x of the marker (the notify time, or a state's start).
    pub anchor: f32,
    /// Right edge of the range for a state, else the anchor.
    pub range_end: f32,
}

/// Pixel size of a marker's label: `chars * char_px` plus the marker head.
#[derive(Clone, Copy, Debug)]
pub struct LaneMetrics {
    pub px_per_second: f32,
    pub char_px: f32,
    pub head_px: f32,
    pub gap_px: f32,
    pub max_label_px: f32,
}

impl Default for LaneMetrics {
    fn default() -> Self {
        Self { px_per_second: 200.0, char_px: 6.5, head_px: 14.0, gap_px: 4.0, max_label_px: 140.0 }
    }
}

/// Stack markers so labels never overlap: each marker takes the first row
/// whose last occupant ends (plus a gap) before this one starts. Order is by
/// time then id so the layout is stable while dragging a neighbour. Returns
/// the placements and the number of rows used.
pub fn lane_layout(entries: &[LaneEntry], metrics: &LaneMetrics) -> (Vec<LanePlacement>, usize) {
    let mut order: Vec<&LaneEntry> = entries.iter().collect();
    order.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.id.cmp(&b.id)));
    let mut row_end: Vec<f32> = Vec::new();
    let mut out = Vec::with_capacity(order.len());
    for e in order {
        let anchor = e.start * metrics.px_per_second;
        let range_end = e.end * metrics.px_per_second;
        let label = (e.label_chars as f32 * metrics.char_px).min(metrics.max_label_px);
        // an instant notify occupies head + label to the right of its anchor;
        // a state occupies at least its range, and its label if longer
        let x0 = anchor - metrics.head_px * 0.5;
        let x1 = match e.kind {
            NotifyKind::Notify => anchor + metrics.head_px * 0.5 + label,
            NotifyKind::State => range_end.max(anchor + label + metrics.head_px),
        };
        let row = row_end.iter().position(|end| *end + metrics.gap_px <= x0).unwrap_or(row_end.len());
        if row == row_end.len() {
            row_end.push(x1);
        } else {
            row_end[row] = x1;
        }
        out.push(LanePlacement { id: e.id.clone(), kind: e.kind, row, x0, x1, anchor, range_end });
    }
    let rows = row_end.len();
    (out, rows)
}

/// Marker under a pixel position, nearest anchor first. `hit_px` is the
/// half-width of the target; the row must match the pointer's row.
pub fn hit_marker<'a>(placements: &'a [LanePlacement], x: f32, row: usize, hit_px: f32) -> Option<&'a LanePlacement> {
    placements
        .iter()
        .filter(|p| p.row == row && x >= p.anchor - hit_px && x <= p.x1.max(p.anchor + hit_px))
        .min_by(|a, b| (a.anchor - x).abs().total_cmp(&(b.anchor - x).abs()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Owner {
        notifies: Vec<AnimationNotify>,
        states: Vec<AnimationNotifyState>,
        schemas: Vec<NotifyPayloadSchema>,
    }

    impl Owner {
        fn new() -> Self {
            Self { notifies: vec![], states: vec![], schemas: vec![] }
        }
        fn view(&mut self) -> NotifyView<'_> {
            NotifyView { notifies: &mut self.notifies, states: &mut self.states, schemas: &mut self.schemas }
        }
    }

    #[test]
    fn notifies_and_states_are_clamped_moved_resized_and_deleted() {
        let mut o = Owner::new();
        let mut v = o.view();
        let n = v.add_notify("Footstep", 9.0, 2.0);
        assert_eq!(v.notifies[0].time, 2.0, "clamped to the clip");
        assert!(v.move_to(&n, 0.5, 2.0));
        assert!(!v.move_to(&n, 0.5, 2.0));
        let s = v.add_state("Window", 0.5, 1.5, 2.0);
        assert!(v.move_to(&s, 1.8, 2.0));
        assert_eq!((v.states[0].start, v.states[0].end), (1.0, 2.0), "a slid state keeps its length and stays inside");
        assert!(v.resize_state(&s, Some(1.9), Some(1.0), 2.0));
        assert!(v.states[0].end - v.states[0].start >= MIN_STATE_LENGTH - 1e-6);
        assert_eq!(v.kind_of(&n), Some(NotifyKind::Notify));
        assert_eq!(v.kind_of(&s), Some(NotifyKind::State));
        assert!(v.delete(&n) && v.delete(&s) && !v.delete(&s));
    }

    #[test]
    fn payload_fields_follow_the_bound_schema() {
        let mut o = Owner::new();
        let mut v = o.view();
        let n = v.add_notify("Footstep", 0.5, 2.0);
        assert!(v.set_field(&n, "1bad", NotifyValue::Bool(true)).is_err());
        assert!(v.set_field(&n, "loud", NotifyValue::Bool(true)).unwrap(), "free-form until a schema is bound");
        let schema = v.add_schema("Foot").unwrap();
        assert!(v.add_schema("Foot").is_err());
        v.add_schema_field(&schema, "surface", NotifyFieldType::String, true).unwrap();
        v.add_schema_field(&schema, "volume", NotifyFieldType::Float, false).unwrap();
        assert!(v.add_schema_field(&schema, "volume", NotifyFieldType::Float, false).is_err(), "duplicate field");
        assert!(v.add_schema_field(&schema, "entity", NotifyFieldType::Int, false).is_err(), "reserved name");
        v.set_schema_default(&schema, "volume", Some(NotifyValue::Float(0.8))).unwrap();
        assert!(v.set_schema_default(&schema, "volume", Some(NotifyValue::Int(1))).is_err(), "default must match the type");
        assert!(v.set_schema(&n, Some(&schema)).unwrap());
        assert!(v.fields(&n).unwrap().iter().any(|f| f.name == "volume"), "defaults filled in");
        // the free-form field is now undeclared and the required one missing
        let codes: Vec<_> = v.diagnostics(2.0).iter().map(|d| d.message.clone()).collect();
        assert_eq!(codes.len(), 1);
        assert!(v.set_field(&n, "loud", NotifyValue::Bool(false)).is_err(), "undeclared names are refused");
        assert!(v.set_field(&n, "volume", NotifyValue::Int(1)).unwrap(), "an int is coerced to the declared float");
        assert_eq!(v.fields(&n).unwrap().iter().find(|f| f.name == "volume").unwrap().value, NotifyValue::Float(1.0));
        assert!(v.remove_undeclared_fields(&n));
        v.set_field(&n, "surface", NotifyValue::String("Stone".into())).unwrap();
        assert!(v.diagnostics(2.0).is_empty());
        assert!(v.delete_schema(&schema).is_err(), "a used schema stays");
        v.set_schema(&n, None).unwrap();
        assert!(v.delete_schema(&schema).unwrap());
    }

    #[test]
    fn targets_ticks_and_disabled_are_validated() {
        let mut o = Owner::new();
        let mut v = o.view();
        let n = v.add_notify("Hit", 0.1, 1.0);
        let s = v.add_state("Trail", 0.2, 0.6, 1.0);
        assert!(v.set_target(&n, NotifyTarget::ClassVariable("not valid".into())).is_err());
        assert!(v.set_target(&n, NotifyTarget::ClassVariable("Weapon".into())).unwrap());
        assert!(v.set_tick_interval(&s, Some(0.0)).is_err());
        assert!(v.set_tick_interval(&s, Some(0.05)).unwrap());
        assert!(v.set_disabled(&s, true) && !v.set_disabled(&s, true));
        assert!(!v.set_disabled(&n, true), "instant notifies have no disabled flag");
        assert!(!v.rename(&n, "  "));
        assert!(v.rename(&n, "Impact"));
    }

    #[test]
    fn diagnostics_flag_markers_past_the_end_with_a_repair() {
        let mut o = Owner::new();
        let mut v = o.view();
        let n = v.add_notify("Late", 3.0, 5.0);
        let s = v.add_state("Long", 1.0, 4.0, 5.0);
        let d = v.diagnostics(2.0);
        assert_eq!(d.len(), 2);
        assert!(d.iter().all(|d| d.repair.is_some()));
        assert!(d.iter().any(|d| d.location.as_deref() == Some(n.as_str())) && d.iter().any(|d| d.location.as_deref() == Some(s.as_str())));
    }

    fn entry(id: &str, kind: NotifyKind, start: f32, end: f32, chars: usize) -> LaneEntry {
        LaneEntry { id: id.into(), kind, start, end, label_chars: chars }
    }

    #[test]
    fn dense_markers_stack_instead_of_overlapping() {
        let m = LaneMetrics { px_per_second: 100.0, char_px: 6.0, head_px: 10.0, gap_px: 2.0, max_label_px: 100.0 };
        // three 8-char labels (48 px) within 0.1 s of each other must take three rows
        let entries = [
            entry("a", NotifyKind::Notify, 0.50, 0.50, 8),
            entry("b", NotifyKind::Notify, 0.55, 0.55, 8),
            entry("c", NotifyKind::Notify, 0.60, 0.60, 8),
            entry("d", NotifyKind::Notify, 2.00, 2.00, 8),
        ];
        let (placed, rows) = lane_layout(&entries, &m);
        assert_eq!(rows, 3);
        let row = |id: &str| placed.iter().find(|p| p.id == id).unwrap().row;
        assert_eq!((row("a"), row("b"), row("c")), (0, 1, 2));
        assert_eq!(row("d"), 0, "far away markers reuse the first row");
        // nothing on one row overlaps
        for r in 0..rows {
            let mut on: Vec<_> = placed.iter().filter(|p| p.row == r).collect();
            on.sort_by(|a, b| a.x0.total_cmp(&b.x0));
            for pair in on.windows(2) {
                assert!(pair[0].x1 <= pair[1].x0, "row {r} overlaps");
            }
        }
    }

    #[test]
    fn states_occupy_their_range_and_layout_is_stable_for_equal_times() {
        let m = LaneMetrics::default();
        let entries = [
            entry("s", NotifyKind::State, 0.2, 1.2, 4),
            entry("n", NotifyKind::Notify, 0.5, 0.5, 4),
            entry("z", NotifyKind::Notify, 0.5, 0.5, 4),
        ];
        let (placed, rows) = lane_layout(&entries, &m);
        assert_eq!(rows, 3, "a notify inside a state's range cannot share its row");
        let (again, _) = lane_layout(&[entries[2].clone(), entries[0].clone(), entries[1].clone()], &m);
        let key = |v: &[LanePlacement]| {
            let mut k: Vec<_> = v.iter().map(|p| (p.id.clone(), p.row)).collect();
            k.sort();
            k
        };
        assert_eq!(key(&placed), key(&again), "input order does not change the layout");
    }

    #[test]
    fn hit_testing_picks_the_marker_under_the_pointer_on_its_row() {
        let m = LaneMetrics { px_per_second: 100.0, ..LaneMetrics::default() };
        let entries = [entry("a", NotifyKind::Notify, 1.0, 1.0, 4), entry("b", NotifyKind::Notify, 1.05, 1.05, 4)];
        let (placed, _) = lane_layout(&entries, &m);
        assert_eq!(hit_marker(&placed, 101.0, 0, 8.0).map(|p| p.id.as_str()), Some("a"));
        assert_eq!(hit_marker(&placed, 106.0, 1, 8.0).map(|p| p.id.as_str()), Some("b"));
        assert!(hit_marker(&placed, 300.0, 0, 8.0).is_none());
    }
}
