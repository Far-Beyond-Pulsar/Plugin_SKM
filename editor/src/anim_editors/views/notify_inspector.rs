//! The inspector for a selected notify or notify state, shared by the Sequence
//! and Montage editors: name, time or range, target, payload schema, payload
//! fields and (at asset level) the payload schema declarations. Every control
//! edits the owning asset through [`NotifyView`]; nothing needs external JSON.

use gpui::*;
use skeletal_assets::notify_schema::{AnimationNotifyState, NotifyFieldType, NotifyPayloadSchema};
use skeletal_assets::notifies::{AnimationNotify, NotifyField, NotifyTarget, NotifyValue};
use ui::button::{Button, ButtonVariants};
use ui::{h_flex, v_flex, ActiveTheme, IconName, Sizable, StyledExt};

use crate::anim_editors::montage_model::MontageModel;
use crate::anim_editors::notify_edit::NotifyView;

use super::chrome::{edit, fmt_f32, labelled, parse_f32, section, EditorBody, EditorHost, EditorModel};

/// A model whose document holds notify records.
pub trait NotifyDoc {
    fn notify_data(&self) -> (&[AnimationNotify], &[AnimationNotifyState], &[NotifyPayloadSchema]);
    /// The end of the timeline notifies live on.
    fn notify_limit(&self) -> f32;
    /// Run a notify command as one undo step.
    fn with_notify<R>(&mut self, label: &str, key: Option<&str>, f: impl FnOnce(&mut NotifyView<'_>, f32) -> (bool, R)) -> R;
}

impl NotifyDoc for MontageModel {
    fn notify_data(&self) -> (&[AnimationNotify], &[AnimationNotifyState], &[NotifyPayloadSchema]) {
        let a = self.asset();
        (&a.notifies, &a.notify_states, &a.notify_schemas)
    }
    fn notify_limit(&self) -> f32 {
        self.timeline_limit()
    }
    fn with_notify<R>(&mut self, label: &str, key: Option<&str>, f: impl FnOnce(&mut NotifyView<'_>, f32) -> (bool, R)) -> R {
        self.notify_edit(label, key, f)
    }
}

fn value_text(v: &NotifyValue) -> String {
    match v {
        NotifyValue::Bool(b) => b.to_string(),
        NotifyValue::Int(i) => i.to_string(),
        NotifyValue::Float(f) => fmt_f32(*f as f32),
        NotifyValue::String(s) => s.clone(),
    }
}

fn parse_value(ty: NotifyFieldType, text: &str) -> Result<NotifyValue, String> {
    match ty {
        NotifyFieldType::Bool => text.trim().parse().map(NotifyValue::Bool).map_err(|_| "enter true or false".to_string()),
        NotifyFieldType::Int => text.trim().parse().map(NotifyValue::Int).map_err(|_| "enter a whole number".to_string()),
        NotifyFieldType::Float => text.trim().parse::<f64>().ok().filter(|v| v.is_finite()).map(NotifyValue::Float).ok_or_else(|| "enter a number".to_string()),
        NotifyFieldType::String => Ok(NotifyValue::String(text.to_string())),
    }
}

/// Apply a committed field of the notify inspector. `None` when `key` is not one.
pub fn commit<M: NotifyDoc>(m: &mut M, id: &str, key: &str, text: &str) -> Option<Result<(), String>> {
    let number = || parse_f32(text).ok_or_else(|| "enter a number".to_string());
    let result = match key {
        "n.name" => {
            m.with_notify("Rename notify", Some(&format!("nname:{id}")), |v, _| {
                let c = v.rename(id, text);
                (c, ())
            });
            Ok(())
        }
        "n.time" => number().map(|t| {
            m.with_notify("Move notify", Some(&format!("nmove:{id}")), |v, limit| {
                let c = v.move_to(id, t, limit);
                (c, ())
            })
        }),
        "n.start" | "n.end" => number().map(|t| {
            m.with_notify("Resize notify state", Some(&format!("nsize:{id}")), |v, limit| {
                let c = if key == "n.start" { v.resize_state(id, Some(t), None, limit) } else { v.resize_state(id, None, Some(t), limit) };
                (c, ())
            })
        }),
        "n.tick" => {
            let interval = if text.trim().is_empty() { Ok(None) } else { number().map(Some) };
            interval.and_then(|i| m.with_notify("Set tick interval", None, |v, _| match v.set_tick_interval(id, i) {
                Ok(c) => (c, Ok(())),
                Err(e) => (false, Err(e)),
            }))
        }
        "n.target" => {
            let target = if text.trim().is_empty() { NotifyTarget::Self_ } else { NotifyTarget::ClassVariable(text.trim().to_string()) };
            m.with_notify("Set notify target", None, |v, _| match v.set_target(id, target) {
                Ok(c) => (c, Ok(())),
                Err(e) => (false, Err(e)),
            })
        }
        k if k.starts_with("n.field.") => {
            let name = &k["n.field.".len()..];
            // keep the field's current type; a schema coerces too
            let (current, schema_ty) = {
                let (notifies, states, schemas) = m.notify_data();
                let fields = notifies.iter().find(|n| n.id == id).map(|n| (&n.fields, &n.schema)).or_else(|| states.iter().find(|s| s.id == id).map(|s| (&s.fields, &s.schema)));
                let cur = fields.and_then(|(f, _)| f.iter().find(|f| f.name == name)).map(|f| NotifyFieldType::of(&f.value));
                let sch = fields.and_then(|(_, s)| s.as_ref()).and_then(|sid| schemas.iter().find(|s| &s.id == sid)).and_then(|s| s.fields.iter().find(|d| d.name == name)).map(|d| d.ty);
                (cur, sch)
            };
            let ty = schema_ty.or(current).unwrap_or(NotifyFieldType::String);
            parse_value(ty, text).and_then(|value| {
                m.with_notify("Edit notify payload", Some(&format!("nfield:{id}:{name}")), |v, _| match v.set_field(id, name, value) {
                    Ok(c) => (c, Ok(())),
                    Err(e) => (false, Err(e)),
                })
            })
        }
        k if k.starts_with("n.fname.") => {
            // rename a free-form field: remove then add with the same value
            let old = &k["n.fname.".len()..];
            let new = text.trim().to_string();
            m.with_notify("Rename payload field", None, |v, _| {
                let Some(value) = v.fields(id).and_then(|f| f.iter().find(|f| f.name == old)).map(|f| f.value.clone()) else { return (false, Err("no such field".to_string())) };
                if old == new {
                    return (false, Ok(()));
                }
                if v.fields(id).is_some_and(|f| f.iter().any(|f| f.name == new)) {
                    return (false, Err(format!("a field named `{new}` exists")));
                }
                match v.set_field(id, &new, value) {
                    Ok(_) => {
                        v.remove_field(id, old);
                        (true, Ok(()))
                    }
                    Err(e) => (false, Err(e)),
                }
            })
        }
        "ns.name" => {
            let name = text.trim().to_string();
            m.with_notify("Add payload schema", None, |v, _| match v.add_schema(&name) {
                Ok(_) => (true, Ok(())),
                Err(e) => (false, Err(e)),
            })
        }
        k if k.starts_with("ns.field.") => {
            // "ns.field.<schema id>" adds a field named by the text, typed String
            let schema = &k["ns.field.".len()..];
            let name = text.trim().to_string();
            m.with_notify("Add schema field", None, |v, _| match v.add_schema_field(schema, &name, NotifyFieldType::String, false) {
                Ok(c) => (c, Ok(())),
                Err(e) => (false, Err(e)),
            })
        }
        _ => return None,
    };
    Some(result)
}

/// The inspector body for the notify or state `id`.
pub fn inspector<V>(this: &mut V, id: &str, window: &mut Window, cx: &mut Context<V>) -> AnyElement
where
    V: EditorBody + EditorHost,
    V::Model: NotifyDoc,
{
    let theme = cx.theme().clone();
    let (notify, state, schemas, limit) = {
        let m = this.model();
        let (n, s, sc) = m.notify_data();
        (n.iter().find(|x| x.id == id).cloned(), s.iter().find(|x| x.id == id).cloned(), sc.to_vec(), m.notify_limit())
    };
    let mut body = v_flex().w_full().gap_2();
    let (name, target, schema, fields, is_state) = match (&notify, &state) {
        (Some(n), _) => (n.name.clone(), n.target.clone(), n.schema.clone(), n.fields.clone(), false),
        (_, Some(s)) => (s.name.clone(), s.target.clone(), s.schema.clone(), s.fields.clone(), true),
        _ => return div().p_2().text_xs().text_color(theme.muted_foreground).child("The notify no longer exists.").into_any_element(),
    };
    body = body.child(div().text_sm().font_semibold().text_color(theme.foreground).child(if is_state { "Notify State" } else { "Notify" }));
    body = body.child(this.widgets().text_field("n.name", "Name", &name, window, cx));
    if let Some(n) = &notify {
        body = body.child(this.widgets().text_field("n.time", "Time (s)", &fmt_f32(n.time), window, cx));
    }
    if let Some(s) = &state {
        body = body.child(this.widgets().text_field("n.start", "Start (s)", &fmt_f32(s.start), window, cx));
        body = body.child(this.widgets().text_field("n.end", "End (s)", &fmt_f32(s.end), window, cx));
        body = body.child(this.widgets().text_field("n.tick", "Tick interval (s)", &s.tick_interval.map(fmt_f32).unwrap_or_default(), window, cx));
        let (sid, disabled) = (s.id.clone(), s.disabled);
        body = body.child(labelled(
            "Disabled",
            Button::new("n-disabled").label(if disabled { "Disabled" } else { "Enabled" }).small().outline().on_click(cx.listener(move |this, _, window, cx| {
                let sid = sid.clone();
                edit(this, window, cx, |m| {
                    m.with_notify("Toggle notify state", None, |v, _| (v.set_disabled(&sid, !disabled), ()));
                    Ok(())
                })
            })),
            cx,
        ));
    }
    let target_text = match &target {
        NotifyTarget::Self_ => String::new(),
        NotifyTarget::ClassVariable(v) => v.clone(),
    };
    body = body.child(this.widgets().text_field("n.target", "Target variable", &target_text, window, cx));
    body = body.child(div().text_xs().text_color(theme.muted_foreground).child("Leave the target empty to notify the owning component."));

    // payload schema
    let current_schema = schema.clone();
    let schema_ids: Vec<Option<String>> = std::iter::once(None).chain(schemas.iter().map(|s| Some(s.id.clone()))).collect();
    let schema_name = schema.as_ref().and_then(|sid| schemas.iter().find(|s| &s.id == sid)).map_or("Free-form".to_string(), |s| s.name.clone());
    let nid = id.to_string();
    body = body.child(labelled(
        "Payload schema",
        Button::new("n-schema").label(schema_name).small().outline().tooltip("Cycle through the declared schemas").on_click(cx.listener(move |this, _, window, cx| {
            let next = schema_ids.iter().position(|s| *s == current_schema).map(|p| schema_ids[(p + 1) % schema_ids.len()].clone()).unwrap_or(None);
            let nid = nid.clone();
            edit(this, window, cx, |m| m.with_notify("Set payload schema", None, |v, _| match v.set_schema(&nid, next.as_deref()) {
                Ok(c) => (c, Ok(())),
                Err(e) => (false, Err(e)),
            }))
        })),
        cx,
    ));

    // payload fields
    let declared = schema.as_ref().and_then(|sid| schemas.iter().find(|s| &s.id == sid));
    let mut rows: Vec<AnyElement> = Vec::new();
    if let Some(declared) = declared {
        for def in &declared.fields {
            let current = fields.iter().find(|f| f.name == def.name).map(|f| value_text(&f.value)).or_else(|| def.default.as_ref().map(value_text)).unwrap_or_default();
            let label = format!("{} ({:?}{})", def.name, def.ty, if def.required { ", required" } else { "" });
            if def.ty == NotifyFieldType::Bool {
                let on = current == "true";
                let (nid, name) = (id.to_string(), def.name.clone());
                rows.push(labelled(
                    &label,
                    Button::new(SharedString::from(format!("nf-bool-{}", def.name))).label(if on { "true" } else { "false" }).small().outline().on_click(cx.listener(move |this, _, window, cx| {
                        let (nid, name) = (nid.clone(), name.clone());
                        edit(this, window, cx, |m| m.with_notify("Edit notify payload", None, |v, _| match v.set_field(&nid, &name, NotifyValue::Bool(!on)) {
                            Ok(c) => (c, Ok(())),
                            Err(e) => (false, Err(e)),
                        }))
                    })),
                    cx,
                ));
            } else {
                rows.push(this.widgets().text_field(&format!("n.field.{}", def.name), &label, &current, window, cx));
            }
        }
        // fields the schema does not declare are errors the user can clear
        let undeclared: Vec<&NotifyField> = fields.iter().filter(|f| !declared.fields.iter().any(|d| d.name == f.name)).collect();
        if !undeclared.is_empty() {
            let nid = id.to_string();
            rows.push(
                Button::new("n-undeclared")
                    .label(format!("Remove {} undeclared field(s)", undeclared.len()))
                    .small()
                    .danger()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let nid = nid.clone();
                        edit(this, window, cx, |m| {
                            m.with_notify("Remove undeclared fields", None, |v, _| (v.remove_undeclared_fields(&nid), ()));
                            Ok(())
                        })
                    }))
                    .into_any_element(),
            );
        }
    } else {
        for f in &fields {
            let name = f.name.clone();
            let (nid, rm) = (id.to_string(), name.clone());
            let ty = NotifyFieldType::of(&f.value);
            let value = f.value.clone();
            rows.push(
                h_flex()
                    .w_full()
                    .gap_1()
                    .items_center()
                    .child(div().w(px(90.0)).child(ui::input::TextInput::new(&this.widgets().input(&format!("n.fname.{name}"), &name, window, cx)).small()))
                    .child(div().flex_1().child(ui::input::TextInput::new(&this.widgets().input(&format!("n.field.{name}"), &value_text(&f.value), window, cx)).small()))
                    .child({
                        let (nid, name) = (nid.clone(), name.clone());
                        Button::new(SharedString::from(format!("nf-type-{name}"))).label(format!("{ty:?}")).xsmall().ghost().tooltip("Change the value type").on_click(cx.listener(move |this, _, window, cx| {
                            let next = match value.clone() {
                                NotifyValue::String(s) => s.trim().parse::<i64>().map(NotifyValue::Int).unwrap_or(NotifyValue::Int(0)),
                                NotifyValue::Int(i) => NotifyValue::Float(i as f64),
                                NotifyValue::Float(f) => NotifyValue::Bool(f != 0.0),
                                NotifyValue::Bool(b) => NotifyValue::String(b.to_string()),
                            };
                            let (nid, name) = (nid.clone(), name.clone());
                            edit(this, window, cx, |m| m.with_notify("Change payload type", None, |v, _| match v.set_field(&nid, &name, next) {
                                Ok(c) => (c, Ok(())),
                                Err(e) => (false, Err(e)),
                            }))
                        }))
                    })
                    .child(
                        Button::new(SharedString::from(format!("nf-del-{rm}"))).icon(IconName::Trash).xsmall().ghost().tooltip("Remove field").on_click(cx.listener(move |this, _, window, cx| {
                            let (nid, rm) = (nid.clone(), rm.clone());
                            edit(this, window, cx, |m| {
                                m.with_notify("Remove payload field", None, |v, _| (v.remove_field(&nid, &rm), ()));
                                Ok(())
                            })
                        })),
                    )
                    .into_any_element(),
            );
        }
        let nid = id.to_string();
        let taken: Vec<String> = fields.iter().map(|f| f.name.clone()).collect();
        rows.push(
            Button::new("n-add-field").icon(IconName::Plus).label("Add field").small().ghost().on_click(cx.listener(move |this, _, window, cx| {
                let name = (1..).map(|i| format!("field_{i}")).find(|n| !taken.contains(n)).unwrap_or_default();
                let nid = nid.clone();
                edit(this, window, cx, |m| m.with_notify("Add payload field", None, |v, _| match v.set_field(&nid, &name, NotifyValue::String(String::new())) {
                    Ok(c) => (c, Ok(())),
                    Err(e) => (false, Err(e)),
                }))
            })).into_any_element(),
        );
    }
    body = body.child(section("Payload", rows, cx));
    body = body.child(div().text_xs().text_color(theme.muted_foreground).child(format!("Times are in seconds, 0 to {}.", fmt_f32(limit))));
    let nid = id.to_string();
    body = body.child(Button::new("n-delete").icon(IconName::Trash).label(if is_state { "Delete Notify State" } else { "Delete Notify" }).small().danger().on_click(cx.listener(move |this, _, window, cx| {
        let nid = nid.clone();
        edit(this, window, cx, |m| {
            m.with_notify("Delete notify", None, |v, _| (v.delete(&nid), ()));
            Ok(())
        });
        this.model_mut().clear_selection();
    })));
    body.into_any_element()
}

/// The payload schema declarations of the asset, with add and field controls.
pub fn schema_section<V>(this: &mut V, window: &mut Window, cx: &mut Context<V>) -> AnyElement
where
    V: EditorBody + EditorHost,
    V::Model: NotifyDoc,
{
    let schemas = this.model().notify_data().2.to_vec();
    let theme = cx.theme().clone();
    let mut rows: Vec<AnyElement> = Vec::new();
    for s in &schemas {
        let used = {
            let (n, st, _) = this.model().notify_data();
            n.iter().any(|x| x.schema.as_deref() == Some(s.id.as_str())) || st.iter().any(|x| x.schema.as_deref() == Some(s.id.as_str()))
        };
        let fields = s.fields.iter().map(|f| format!("{} {:?}{}", f.name, f.ty, if f.required { "*" } else { "" })).collect::<Vec<_>>().join(", ");
        let sid = s.id.clone();
        rows.push(
            v_flex()
                .w_full()
                .gap_1()
                .child(
                    h_flex()
                        .w_full()
                        .justify_between()
                        .items_center()
                        .child(div().text_xs().font_semibold().text_color(theme.foreground).child(s.name.clone()))
                        .child({
                            let sid = sid.clone();
                            Button::new(SharedString::from(format!("ns-del-{}", s.id))).icon(IconName::Trash).xsmall().ghost().disabled(used).tooltip(if used { "A notify uses this schema" } else { "Delete schema" }).on_click(cx.listener(move |this, _, window, cx| {
                                let sid = sid.clone();
                                edit(this, window, cx, |m| m.with_notify("Delete payload schema", None, |v, _| match v.delete_schema(&sid) {
                                    Ok(c) => (c, Ok(())),
                                    Err(e) => (false, Err(e)),
                                }))
                            }))
                        }),
                )
                .child(div().text_xs().text_color(theme.muted_foreground).child(if fields.is_empty() { "No fields".to_string() } else { fields }))
                .child(this.widgets().text_field(&format!("ns.field.{}", s.id), "Add field", "", window, cx))
                .into_any_element(),
        );
    }
    rows.push(this.widgets().text_field("ns.name", "New schema", "", window, cx));
    section("Payload schemas", rows, cx)
}

use ui::Disableable;
