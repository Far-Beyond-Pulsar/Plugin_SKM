//! Typed notify payloads and notify states.
//!
//! A [`NotifyPayloadSchema`] declares the fields a notify or notify state
//! carries, with types, defaults and required flags. Sequence and Montage
//! records reference a schema by its stable id; the Blueprint event descriptor
//! and the editor inspector are generated from the same declaration. An
//! [`AnimationNotifyState`] is a ranged event that dispatches begin, tick and
//! end; the instantaneous [`crate::notifies::AnimationNotify`] is unchanged.

use serde::{Deserialize, Serialize};

use crate::notifies::{identifier, NotifyField, NotifyTarget, NotifyValue, RESERVED_NOTIFY_FIELDS};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyFieldType {
    Bool,
    Int,
    Float,
    String,
}

impl NotifyFieldType {
    pub fn of(value: &NotifyValue) -> Self {
        match value {
            NotifyValue::Bool(_) => Self::Bool,
            NotifyValue::Int(_) => Self::Int,
            NotifyValue::Float(_) => Self::Float,
            NotifyValue::String(_) => Self::String,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NotifyFieldDef {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: NotifyFieldType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<NotifyValue>,
    /// A required field with no default must be set by every notify using the
    /// schema.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NotifyPayloadSchema {
    /// Stable id; notifies refer to the schema by it.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub fields: Vec<NotifyFieldDef>,
}

impl NotifyPayloadSchema {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            fields: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.id.trim().is_empty() || self.name.trim().is_empty() {
            return Err("Payload schema id and name cannot be empty".into());
        }
        let mut names = std::collections::HashSet::new();
        for field in &self.fields {
            if !identifier(&field.name) || RESERVED_NOTIFY_FIELDS.contains(&field.name.as_str()) {
                return Err(format!(
                    "Schema `{}` field `{}` needs a nonreserved identifier",
                    self.name, field.name
                ));
            }
            if !names.insert(field.name.as_str()) {
                return Err(format!(
                    "Schema `{}` repeats field `{}`",
                    self.name, field.name
                ));
            }
            if let Some(default) = &field.default {
                if NotifyFieldType::of(default) != field.ty {
                    return Err(format!(
                        "Schema `{}` default of `{}` does not match its type",
                        self.name, field.name
                    ));
                }
                if matches!(default, NotifyValue::Float(v) if !v.is_finite()) {
                    return Err("Payload numbers must be finite".into());
                }
            }
        }
        Ok(())
    }

    /// Check `fields` against the schema: known names, matching types, every
    /// required field set or defaulted.
    pub fn check(&self, fields: &[NotifyField]) -> Result<(), String> {
        for field in fields {
            let def = self
                .fields
                .iter()
                .find(|d| d.name == field.name)
                .ok_or_else(|| {
                    format!(
                        "Field `{}` is not declared by schema `{}`",
                        field.name, self.name
                    )
                })?;
            // An integer literal is a valid float payload; the reverse is not.
            let ok = def.ty == NotifyFieldType::of(&field.value)
                || (def.ty == NotifyFieldType::Float && matches!(field.value, NotifyValue::Int(_)));
            if !ok {
                return Err(format!(
                    "Field `{}` must be {:?} per schema `{}`",
                    field.name, def.ty, self.name
                ));
            }
        }
        for def in &self.fields {
            if def.required && def.default.is_none() && !fields.iter().any(|f| f.name == def.name) {
                return Err(format!(
                    "Required field `{}` of schema `{}` is not set",
                    def.name, self.name
                ));
            }
        }
        Ok(())
    }

    /// `fields` completed with schema defaults, in schema order. This is the
    /// payload a dispatched event carries.
    pub fn resolve(&self, fields: &[NotifyField]) -> Vec<NotifyField> {
        self.fields
            .iter()
            .filter_map(|def| {
                fields
                    .iter()
                    .find(|f| f.name == def.name)
                    .cloned()
                    .map(|mut f| {
                        if let (NotifyFieldType::Float, NotifyValue::Int(i)) = (def.ty, &f.value) {
                            f.value = NotifyValue::Float(*i as f64);
                        }
                        f
                    })
                    .or_else(|| {
                        def.default.clone().map(|value| NotifyField {
                            name: def.name.clone(),
                            value,
                        })
                    })
            })
            .collect()
    }
}

/// Look up `schema` among `schemas` and check `fields` against it. A notify
/// without a schema carries free-form fields, as before.
pub fn check_payload(
    schemas: &[NotifyPayloadSchema],
    schema: &Option<String>,
    fields: &[NotifyField],
) -> Result<(), String> {
    let Some(id) = schema else { return Ok(()) };
    let schema = schemas
        .iter()
        .find(|s| &s.id == id)
        .ok_or_else(|| format!("Payload schema `{id}` does not exist"))?;
    schema.check(fields)
}

/// A ranged event: dispatches begin at `start`, tick while inside, and end at
/// `end`, once per playback instance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnimationNotifyState {
    pub id: String,
    pub name: String,
    pub start: f32,
    pub end: f32,
    #[serde(default, skip_serializing_if = "NotifyTarget::is_self")]
    pub target: NotifyTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<NotifyField>,
    /// Seconds between tick events; `None` ticks on every update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick_interval: Option<f32>,
    /// A disabled state is kept in the asset but never dispatches.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
}

impl AnimationNotifyState {
    pub fn new(name: impl Into<String>, start: f32, end: f32) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            start,
            end,
            target: NotifyTarget::Self_,
            schema: None,
            fields: Vec::new(),
            tick_interval: None,
            disabled: false,
        }
    }

    pub fn duration(&self) -> f32 {
        self.end - self.start
    }

    pub fn validate(&self, duration: f32) -> Result<(), String> {
        if self.id.trim().is_empty() || self.name.trim().is_empty() {
            return Err("Notify state ID and name cannot be empty".into());
        }
        if !self.start.is_finite() || !self.end.is_finite() {
            return Err("Notify state times must be finite".into());
        }
        if self.start < 0.0 || self.end > duration || self.start >= self.end {
            return Err(format!(
                "Notify state `{}` must satisfy 0 <= start < end <= clip duration",
                self.name
            ));
        }
        if let Some(interval) = self.tick_interval {
            if !interval.is_finite() || interval <= 0.0 {
                return Err("Tick interval must be positive".into());
            }
        }
        crate::notifies::validate_target_and_fields(&self.target, &self.fields)
    }
}
