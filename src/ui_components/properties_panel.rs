//! Right dock panel: details and transform properties for the selected bone
//! or keyframe.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};

use gpui::prelude::FluentBuilder;
use gpui::*;
use ui::button::{Button, ButtonVariants};
use ui::dock::PanelEvent;
use ui::input::{InputEvent, InputState, NumberInput, TextInput};
use ui::scroll::ScrollbarAxis;
use ui::tag::Tag;
use ui::{h_flex, v_flex, ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt};

use crate::editor::panel::{EditorMode, SkeletalAnimEditorPanel};
use crate::editor::transform_inputs::TransformGroup;
use crate::style::{AXIS_X, AXIS_Y, AXIS_Z};
use crate::ui_components::chrome::{empty_message, panel_header, MONO};
use skeletal_assets::notifies::{NotifyField, NotifyTarget, NotifyValue};

pub struct BonePropertiesPanel {
    editor: WeakEntity<SkeletalAnimEditorPanel>,
    focus_handle: FocusHandle,
    collapsed: HashSet<&'static str>,
    /// What the panel shows, copied out of the editor. Rendering reads only
    /// this: a view that reads an entity while rendering is rebuilt on every
    /// notify of it, and the editor notifies on every playback frame.
    details: Option<Details>,
    notify: Option<Marker>,
    /// End, tick and length inputs of a selected state, keyed `<id>:<what>`.
    state_inputs: HashMap<String, (Entity<InputState>, Subscription)>,
    notify_name_input: Option<(String, Entity<InputState>)>,
    _notify_name_subscription: Option<Subscription>,
    notify_time_input: Option<(String, Entity<InputState>)>,
    _notify_time_subscription: Option<Subscription>,
    notify_target_input: Option<(String, Entity<InputState>)>,
    _notify_target_subscription: Option<Subscription>,
    notify_field_inputs: HashMap<String, Entity<InputState>>,
    notify_field_subscriptions: HashMap<String, Subscription>,
    notify_field_owner: Option<(String, usize)>,
    signature: u64,
    _editor_observer: Option<Subscription>,
}

/// A selected notify (instant) or notify state (ranged), copied out of the editor.
#[derive(Clone)]
struct Marker {
    id: String,
    name: String,
    /// The notify time, or a state's start.
    time: f32,
    /// A state's end; `None` for an instant notify.
    end: Option<f32>,
    tick: Option<f32>,
    disabled: bool,
    target: NotifyTarget,
    fields: Vec<NotifyField>,
}

/// Snapshot of what the panel shows, read once per render.
struct Details {
    socket: bool,
    sequence: bool,
    id: String,
    name: String,
    parent: Option<String>,
    child_count: usize,
    key_count: Option<usize>,
    /// Frame of the selected keyframe, if one is selected.
    selected_key: Option<(usize, i32, f32)>,
    keyed: bool,
    /// Animated bone with the playhead between keys: edits need a key first.
    locked: bool,
    /// Where the playhead sits relative to the bone's keys.
    timing: String,
    world: Option<[f32; 3]>,
    translation: [Entity<InputState>; 3],
    rotation: [Entity<InputState>; 3],
    scale: [Entity<InputState>; 3],
}

impl BonePropertiesPanel {
    pub fn new(editor: WeakEntity<SkeletalAnimEditorPanel>, cx: &mut Context<Self>) -> Self {
        Self {
            editor,
            focus_handle: cx.focus_handle(),
            collapsed: HashSet::new(),
            details: None,
            notify: None,
            state_inputs: HashMap::new(),
            notify_name_input: None,
            _notify_name_subscription: None,
            notify_time_input: None,
            _notify_time_subscription: None,
            notify_target_input: None,
            _notify_target_subscription: None,
            notify_field_inputs: HashMap::new(),
            notify_field_subscriptions: HashMap::new(),
            notify_field_owner: None,
            signature: 0,
            _editor_observer: None,
        }
    }

    /// Start following the editor. Call once, outside of render.
    pub fn attach(
        &mut self,
        state: &SkeletalAnimEditorPanel,
        editor: &Entity<SkeletalAnimEditorPanel>,
        cx: &mut Context<Self>,
    ) {
        self.signature = Self::signature(state);
        self.details = Self::capture(state);
        self.notify = Self::capture_notify(state);
        self._editor_observer = Some(cx.observe(editor, |this, editor, cx| {
            let e = editor.read(cx);
            let signature = Self::signature(e);
            if signature != this.signature {
                this.signature = signature;
                this.details = Self::capture(e);
                this.notify = Self::capture_notify(e);
                cx.notify();
            }
        }));
    }

    /// Hash of the editor state this panel shows. While playing, the playhead
    /// is left out so the panel doesn't rebuild every frame.
    fn signature(e: &SkeletalAnimEditorPanel) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        e.selected_bone.hash(&mut h);
        e.selected_socket.hash(&mut h);
        e.selected_keyframe.hash(&mut h);
        e.selected_notify.hash(&mut h);
        if let Some(marker) = Self::capture_notify(e) {
            marker.name.hash(&mut h);
            marker.time.to_bits().hash(&mut h);
            marker.end.map(f32::to_bits).hash(&mut h);
            marker.tick.map(f32::to_bits).hash(&mut h);
            marker.disabled.hash(&mut h);
            format!("{:?}{:?}", marker.target, marker.fields).hash(&mut h);
        }
        e.playback.playing.hash(&mut h);
        e.skeleton_revision.hash(&mut h);
        e.transform_editable().hash(&mut h);
        if !e.playback.playing {
            ((e.playback.time * 1000.0).round() as i64).hash(&mut h);
        }
        if let Some(track) = e
            .selected_bone
            .as_ref()
            .and_then(|id| e.animation.track(id))
        {
            for k in &track.keyframes {
                k.time.to_bits().hash(&mut h);
            }
        }
        h.finish()
    }

    fn capture_notify(e: &SkeletalAnimEditorPanel) -> Option<Marker> {
        let id = e.selected_notify.as_ref()?;
        if let Some(n) = e.animation_features.notifies.iter().find(|n| &n.id == id) {
            return Some(Marker {
                id: n.id.clone(),
                name: n.name.clone(),
                time: n.time,
                end: None,
                tick: None,
                disabled: false,
                target: n.target.clone(),
                fields: n.fields.clone(),
            });
        }
        e.animation_features
            .notify_states
            .iter()
            .find(|s| &s.id == id)
            .map(|s| Marker {
                id: s.id.clone(),
                name: s.name.clone(),
                time: s.start,
                end: Some(s.end),
                tick: s.tick_interval,
                disabled: s.disabled,
                target: s.target.clone(),
                fields: s.fields.clone(),
            })
    }

    fn capture(e: &SkeletalAnimEditorPanel) -> Option<Details> {
        let playing = e.playback.playing;
        if let Some(socket) = e
            .selected_socket
            .as_deref()
            .and_then(|id| e.sockets.get(id))
        {
            return Some(Details {
                socket: true,
                sequence: false,
                id: socket.id.clone(),
                name: socket.name.clone(),
                parent: e.skeleton.bone(&socket.bone_id).map(|b| b.name.clone()),
                child_count: 0,
                key_count: None,
                selected_key: None,
                keyed: false,
                locked: e.mode != EditorMode::Skeleton,
                timing: "Bone-local attachment transform".into(),
                world: if playing {
                    None
                } else {
                    socket
                        .world_transform(&e.skeleton, &e.animation, e.playback.time)
                        .map(|m| m.transform_point(crate::core::Vec3::ZERO).to_array())
                },
                translation: e.transform_inputs.translation.clone(),
                rotation: e.transform_inputs.rotation.clone(),
                scale: e.transform_inputs.scale.clone(),
            });
        }
        let id = e.selected_bone.clone()?;
        let bone = e.skeleton.bone(&id)?;
        let track = e.animation.track(&id);
        let keyed = e.selected_keyframe.is_some();

        let selected_key = e.selected_keyframe.as_ref().and_then(|(bone_id, index)| {
            let kf = e.animation.track(bone_id)?.keyframes.get(*index)?;
            Some((index + 1, e.animation.time_to_frame(kf.time), kf.time))
        });

        let timing = match track {
            _ if playing => "Playing".to_string(),
            None if e.mode == EditorMode::Skeleton => "Edits change the bind pose".to_string(),
            None => "Not animated — showing the skeleton's bind pose".to_string(),
            Some(t) if t.keyframes.is_empty() => "No keys".to_string(),
            Some(t) => {
                let time = e.playback.time;
                let frame = |s: f32| e.animation.time_to_frame(s);
                match e.keyframe_at_playhead(&id) {
                    Some(_) => format!("Keyed at frame {}", e.current_frame()),
                    None => {
                        let prev = t.keyframes.iter().rev().find(|k| k.time < time);
                        let next = t.keyframes.iter().find(|k| k.time > time);
                        match (prev, next) {
                            (Some(a), Some(b)) => {
                                format!(
                                    "Interpolating frames {} → {}",
                                    frame(a.time),
                                    frame(b.time)
                                )
                            }
                            (Some(a), None) => {
                                format!("Holding last key (frame {})", frame(a.time))
                            }
                            (None, Some(b)) => {
                                format!("Holding first key (frame {})", frame(b.time))
                            }
                            (None, None) => "No keys".to_string(),
                        }
                    }
                }
            }
        };

        Some(Details {
            socket: false,
            sequence: e.mode == EditorMode::Sequence,
            parent: bone
                .parent
                .as_ref()
                .and_then(|p| e.skeleton.bone(p))
                .map(|p| p.name.clone()),
            child_count: e.skeleton.children_of(&id).len(),
            key_count: track.map(|t| t.keyframes.len()),
            selected_key,
            keyed,
            locked: !e.transform_editable(),
            timing,
            world: if playing {
                None
            } else {
                e.world_position(&id).map(|p| [p.x, p.y, p.z])
            },
            translation: e.transform_inputs.translation.clone(),
            rotation: e.transform_inputs.rotation.clone(),
            scale: e.transform_inputs.scale.clone(),
            name: bone.name.clone(),
            id,
        })
    }

    fn chip(&self, icon: IconName, text: String, color: Hsla, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .gap_1()
            .px_1p5()
            .py_0p5()
            .items_center()
            .rounded(px(5.0))
            .bg(color.opacity(0.14))
            .border_1()
            .border_color(color.opacity(0.35))
            .child(Icon::new(icon).size(px(11.0)).text_color(color))
            .child(
                div()
                    .text_xs()
                    .text_color(theme.foreground.opacity(0.9))
                    .child(text),
            )
    }

    fn render_bone_card(&self, d: &Details, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let location = match &d.parent {
            Some(parent) => self
                .chip(
                    IconName::ArrowUp,
                    format!("Parent: {parent}"),
                    theme.info,
                    cx,
                )
                .into_any_element(),
            None => self
                .chip(IconName::Home, "Root".to_string(), theme.warning, cx)
                .into_any_element(),
        };
        let animation = match d.key_count {
            _ if d.socket => self
                .chip(IconName::Link, "Follows bone".into(), theme.info, cx)
                .into_any_element(),
            Some(n) => self
                .chip(IconName::Keyframes, format!("{n} keys"), theme.success, cx)
                .into_any_element(),
            None => self
                .chip(
                    IconName::Minus,
                    "Static".to_string(),
                    theme.muted_foreground,
                    cx,
                )
                .into_any_element(),
        };

        v_flex()
            .w_full()
            .p_3()
            .gap_2()
            .border_b_1()
            .border_color(theme.border.opacity(0.6))
            .child(
                v_flex()
                    .gap_0p5()
                    .child(
                        div()
                            .text_base()
                            .font_semibold()
                            .text_color(theme.foreground)
                            .child(d.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_family(MONO)
                            .text_color(theme.muted_foreground)
                            .child(d.id.clone()),
                    ),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_1()
                    .child(location)
                    .when(d.child_count > 0, |row| {
                        row.child(self.chip(
                            IconName::Component,
                            format!(
                                "{} {}",
                                d.child_count,
                                if d.child_count == 1 {
                                    "child"
                                } else {
                                    "children"
                                }
                            ),
                            theme.muted_foreground,
                            cx,
                        ))
                    })
                    .child(animation),
            )
    }

    /// Previous key / key-at-playhead toggle / next key, with a status line.
    fn render_key_card(&self, d: &Details, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let keyed_here = d.keyed;
        let (e_prev, e_next, e_toggle) = (
            self.editor.clone(),
            self.editor.clone(),
            self.editor.clone(),
        );

        v_flex()
            .w_full()
            .px_3()
            .py_2p5()
            .gap_2()
            .border_b_1()
            .border_color(theme.border.opacity(0.6))
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        Button::new("props-prev-key")
                            .icon(IconName::FastArrowLeft)
                            .ghost()
                            .small()
                            .tooltip("Previous keyframe")
                            .on_click(move |_, window, cx| {
                                let _ = e_prev.update(cx, |e, cx| e.goto_prev_keyframe(window, cx));
                            }),
                    )
                    .child(
                        div().flex_1().child(
                            Button::new("props-toggle-key")
                                .icon(if keyed_here {
                                    IconName::Keyframe
                                } else {
                                    IconName::KeyframePlus
                                })
                                .label(if keyed_here { "Remove key" } else { "Add key" })
                                .small()
                                .w_full()
                                .when(keyed_here, |b| b.danger())
                                .tooltip(if keyed_here {
                                    "Remove the keyframe at the playhead"
                                } else {
                                    "Capture the current pose as a keyframe (K)"
                                })
                                .on_click(move |_, window, cx| {
                                    let _ = e_toggle.update(cx, |e, cx| {
                                        if keyed_here {
                                            e.delete_keyframe_at_playhead(window, cx);
                                        } else {
                                            e.insert_keyframe(window, cx);
                                        }
                                    });
                                }),
                        ),
                    )
                    .child(
                        Button::new("props-next-key")
                            .icon(IconName::FastArrowRight)
                            .ghost()
                            .small()
                            .tooltip("Next keyframe")
                            .on_click(move |_, window, cx| {
                                let _ = e_next.update(cx, |e, cx| e.goto_next_keyframe(window, cx));
                            }),
                    ),
            )
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        Icon::new(if keyed_here {
                            IconName::Keyframe
                        } else {
                            IconName::Info
                        })
                        .size(px(12.0))
                        .text_color(if keyed_here {
                            theme.warning
                        } else {
                            theme.muted_foreground
                        }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(match d.selected_key {
                                Some((n, frame, time)) => {
                                    format!("Key #{n} · frame {frame} · {time:.3}s")
                                }
                                None => d.timing.clone(),
                            }),
                    ),
            )
    }

    fn axis_row(
        &self,
        axis: &'static str,
        color: Hsla,
        input: &Entity<InputState>,
        unit: &'static str,
        locked: bool,
        cx: &App,
    ) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .w_full()
            .gap_1p5()
            .items_center()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(20.0))
                    .h(px(24.0))
                    .rounded(px(4.0))
                    .bg(color.opacity(0.18))
                    .border_1()
                    .border_color(color.opacity(0.45))
                    .text_xs()
                    .font_semibold()
                    .font_family(MONO)
                    .text_color(color)
                    .child(axis),
            )
            .child(
                div().flex_1().child(
                    NumberInput::new(input).small().disabled(locked).suffix(
                        div()
                            .pr_1()
                            .text_xs()
                            .font_family(MONO)
                            .text_color(theme.muted_foreground)
                            .child(unit),
                    ),
                ),
            )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_section(
        &self,
        group: TransformGroup,
        title: &'static str,
        icon: IconName,
        unit: &'static str,
        inputs: &[Entity<InputState>; 3],
        locked: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let collapsed = self.collapsed.contains(title);
        let editor = self.editor.clone();

        v_flex()
            .w_full()
            .border_b_1()
            .border_color(theme.border.opacity(0.6))
            .child(
                h_flex()
                    .id(SharedString::from(format!("props-section-{title}")))
                    .w_full()
                    .h(px(30.0))
                    .px_2()
                    .gap_1p5()
                    .items_center()
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.foreground.opacity(0.04)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.collapsed.remove(title) {
                            this.collapsed.insert(title);
                        }
                        cx.notify();
                    }))
                    .child(
                        Icon::new(if collapsed {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .size(px(12.0))
                        .text_color(theme.muted_foreground),
                    )
                    .child(Icon::new(icon).size(px(13.0)).text_color(theme.info))
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .font_semibold()
                            .text_color(theme.foreground)
                            .child(title),
                    )
                    .child(
                        Button::new(SharedString::from(format!("props-reset-{title}")))
                            .debug_selector(|| format!("props-reset-{title}"))
                            .icon(IconName::Undo)
                            .ghost()
                            .xsmall()
                            .disabled(locked)
                            .stop_propagation(true)
                            .tooltip(format!("Reset {}", title.to_lowercase()))
                            .on_click(move |_, window, cx| {
                                let _ = editor
                                    .update(cx, |e, cx| e.reset_transform_group(group, window, cx));
                            }),
                    ),
            )
            .when(!collapsed, |section| {
                section.child(
                    v_flex()
                        .w_full()
                        .px_3()
                        .pb_2p5()
                        .gap_1p5()
                        .child(self.axis_row("X", AXIS_X, &inputs[0], unit, locked, cx))
                        .child(self.axis_row("Y", AXIS_Y, &inputs[1], unit, locked, cx))
                        .child(self.axis_row("Z", AXIS_Z, &inputs[2], unit, locked, cx)),
                )
            })
    }

    fn render_world(&self, world: [f32; 3], cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let cell = |axis: &'static str, color: Hsla, value: f32| {
            h_flex()
                .flex_1()
                .gap_1()
                .items_center()
                .child(
                    div()
                        .text_xs()
                        .font_semibold()
                        .font_family(MONO)
                        .text_color(color)
                        .child(axis),
                )
                .child(
                    div()
                        .text_xs()
                        .font_family(MONO)
                        .text_color(theme.foreground.opacity(0.85))
                        .child(format!("{value:.3}")),
                )
        };
        v_flex()
            .w_full()
            .px_3()
            .py_2()
            .gap_1()
            .child(
                h_flex()
                    .gap_1p5()
                    .items_center()
                    .child(
                        Icon::new(IconName::Globe)
                            .size(px(12.0))
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .text_xs()
                            .font_semibold()
                            .text_color(theme.muted_foreground)
                            .child("World position"),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(cell("X", AXIS_X, world[0]))
                    .child(cell("Y", AXIS_Y, world[1]))
                    .child(cell("Z", AXIS_Z, world[2])),
            )
    }
}

impl EventEmitter<PanelEvent> for BonePropertiesPanel {}

impl Focusable for BonePropertiesPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ui::dock::Panel for BonePropertiesPanel {
    fn panel_name(&self) -> &'static str {
        "skeletal-bone-properties"
    }

    fn title(&self, _window: &Window, _cx: &App) -> AnyElement {
        "Properties".into_any_element()
    }
}

impl Render for BonePropertiesPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        // End time and tick interval of a selected state.
        let state_id = self
            .notify
            .as_ref()
            .filter(|m| m.end.is_some())
            .map(|m| m.id.clone());
        self.state_inputs
            .retain(|key, _| state_id.as_ref().is_some_and(|id| key.starts_with(&format!("{id}:"))));
        if let Some(marker) = self.notify.clone().filter(|m| m.end.is_some()) {
            let end = marker.end.unwrap_or(marker.time);
            self.state_input(
                format!("{}:end", marker.id),
                format!("{end:.3}"),
                window,
                cx,
                |e, id, text| {
                    if let Ok(end) = text.trim().parse::<f32>() {
                        if end.is_finite() {
                            e.resize_notify_state(id, None, Some(end));
                        }
                    }
                },
            );
            self.state_input(
                format!("{}:tick", marker.id),
                marker.tick.map(|t| format!("{t}")).unwrap_or_default(),
                window,
                cx,
                |e, id, text| {
                    let text = text.trim();
                    if text.is_empty() {
                        let _ = e.set_notify_state_tick(id, None);
                    } else if let Ok(interval) = text.parse::<f32>() {
                        let _ = e.set_notify_state_tick(id, Some(interval));
                    }
                },
            );
        }

        let details = self.details.as_ref();
        if let Some(notify) = self.notify.as_ref() {
            if self
                .notify_name_input
                .as_ref()
                .is_none_or(|(id, _)| id != &notify.id)
            {
                let input =
                    cx.new(|cx| InputState::new(window, cx).default_value(notify.name.clone()));
                let editor = self.editor.clone();
                let id = notify.id.clone();
                self._notify_name_subscription = Some(cx.subscribe_in(
                    &input,
                    window,
                    move |_this, input, event: &InputEvent, _window, cx| {
                        if matches!(event, InputEvent::Change) {
                            let name = input.read(cx).value().to_string();
                            if let Some(editor) = editor.upgrade() {
                                editor.update(cx, |e, cx| {
                                    e.set_notify_name(&id, name);
                                    cx.notify();
                                });
                            }
                        }
                    },
                ));
                self.notify_name_input = Some((notify.id.clone(), input));
            }
            if self
                .notify_time_input
                .as_ref()
                .is_none_or(|(id, _)| id != &notify.id)
            {
                let input = cx.new(|cx| {
                    InputState::new(window, cx).default_value(format!("{:.3}", notify.time))
                });
                let editor = self.editor.clone();
                let id = notify.id.clone();
                self._notify_time_subscription = Some(cx.subscribe_in(
                    &input,
                    window,
                    move |_this, input, event: &InputEvent, window, cx| {
                        if matches!(event, InputEvent::Change) {
                            let value = input.read(cx).value().trim().parse::<f32>();
                            if let (Ok(time), Some(editor)) = (value, editor.upgrade()) {
                                if time.is_finite() {
                                    editor.update(cx, |e, cx| {
                                        e.set_notify_time(&id, time);
                                        if let Some(snapped) = e
                                            .animation_features
                                            .notifies
                                            .iter()
                                            .find(|notify| notify.id == id)
                                            .map(|notify| notify.time)
                                            .or_else(|| {
                                                e.animation_features
                                                    .notify_states
                                                    .iter()
                                                    .find(|s| s.id == id)
                                                    .map(|s| s.start)
                                            })
                                        {
                                            e.seek(snapped, window, cx);
                                        }
                                    });
                                }
                            }
                        }
                    },
                ));
                self.notify_time_input = Some((notify.id.clone(), input));
            }
            if let Some((id, input)) = self.notify_time_input.as_ref() {
                if id == &notify.id && !input.read(cx).focus_handle(cx).is_focused(window) {
                    let displayed = input.read(cx).value().trim().parse::<f32>().ok();
                    if displayed.is_none_or(|time| (time - notify.time).abs() > 0.0005) {
                        input.update(cx, |input, cx| {
                            input.set_value(format!("{:.3}", notify.time), window, cx)
                        });
                    }
                }
            }
            if let NotifyTarget::ClassVariable(target) = &notify.target {
                if self
                    .notify_target_input
                    .as_ref()
                    .is_none_or(|(id, _)| id != &notify.id)
                {
                    let input =
                        cx.new(|cx| InputState::new(window, cx).default_value(target.clone()));
                    let editor = self.editor.clone();
                    let id = notify.id.clone();
                    self._notify_target_subscription = Some(cx.subscribe_in(
                        &input,
                        window,
                        move |_this, input, event: &InputEvent, _window, cx| {
                            if matches!(event, InputEvent::Change) {
                                let target = input.read(cx).value().to_string();
                                if let Some(editor) = editor.upgrade() {
                                    editor.update(cx, |e, cx| {
                                        e.set_notify_target_name(&id, target);
                                        cx.notify();
                                    });
                                }
                            }
                        },
                    ));
                    self.notify_target_input = Some((notify.id.clone(), input));
                }
            } else {
                self.notify_target_input = None;
                self._notify_target_subscription = None;
            }
        } else {
            self.notify_name_input = None;
            self._notify_name_subscription = None;
            self.notify_time_input = None;
            self._notify_time_subscription = None;
            self.notify_target_input = None;
            self._notify_target_subscription = None;
        }

        let badge = match &details {
            _ if self.notify.as_ref().is_some_and(|m| m.end.is_some()) => {
                Some(Tag::info().small().child("Notify state"))
            }
            _ if self.notify.is_some() => Some(Tag::warning().small().child("Notify")),
            Some(d) if d.socket => Some(Tag::info().small().child("Socket")),
            Some(d) if d.selected_key.is_some() => Some(Tag::warning().small().child("Keyframe")),
            Some(_) => Some(Tag::info().small().child("Bone")),
            None => None,
        };
        let header = panel_header(
            IconName::Settings,
            "Details",
            badge.map(|b| b.into_any_element()),
            cx,
        );

        let notify_snapshot = self.notify.clone();
        let body: AnyElement = if let Some(notify) = notify_snapshot.as_ref() {
            self.render_notify_card(notify, window, cx)
                .id("notify-properties-scroll")
                .scrollable(ScrollbarAxis::Vertical)
                .into_any_element()
        } else {
            match details {
                None => div()
                    .flex_1()
                    .min_h_0()
                    .child(empty_message(
                        IconName::Component,
                        "Nothing selected",
                        "Pick a bone in the hierarchy or click a joint in the viewport",
                        cx,
                    ))
                    .into_any_element(),
                Some(d) => v_flex()
                    .id("properties-scroll")
                    .flex_1()
                    .min_h_0()
                    .scrollable(ScrollbarAxis::Vertical)
                    .child(self.render_bone_card(d, cx))
                    .when(d.sequence, |col| col.child(self.render_key_card(d, cx)))
                    .when(d.locked, |col| {
                        col.child(
                            h_flex()
                                .w_full()
                                .mx_3()
                                .mt_2()
                                .px_2()
                                .py_1p5()
                                .gap_1p5()
                                .items_center()
                                .rounded(px(6.0))
                                .bg(theme.warning.opacity(0.10))
                                .border_1()
                                .border_color(theme.warning.opacity(0.35))
                                .child(
                                    Icon::new(IconName::Info)
                                        .size(px(13.0))
                                        .text_color(theme.warning),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .text_xs()
                                        .text_color(theme.foreground.opacity(0.9))
                                        .child(if d.socket {
                                            "Open the skeleton asset to edit this socket."
                                        } else {
                                            "Add a key at the playhead to edit this pose."
                                        }),
                                ),
                        )
                    })
                    .child(self.render_section(
                        TransformGroup::Translation,
                        "Translation",
                        IconName::Home,
                        "m",
                        &d.translation,
                        d.locked,
                        cx,
                    ))
                    .child(self.render_section(
                        TransformGroup::Rotation,
                        "Rotation",
                        IconName::RotateCameraLeft,
                        "°",
                        &d.rotation,
                        d.locked,
                        cx,
                    ))
                    .child(self.render_section(
                        TransformGroup::Scale,
                        "Scale",
                        IconName::Expand,
                        "×",
                        &d.scale,
                        d.locked,
                        cx,
                    ))
                    .when_some(d.world, |col, world| {
                        col.child(self.render_world(world, cx))
                    })
                    .into_any_element(),
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

impl BonePropertiesPanel {
    fn render_notify_card(
        &mut self,
        notify: &Marker,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = cx.theme().clone();
        let field_owner = (notify.id.clone(), notify.fields.len());
        if self.notify_field_owner.as_ref() != Some(&field_owner) {
            self.notify_field_inputs.clear();
            self.notify_field_subscriptions.clear();
            self.notify_field_owner = Some(field_owner);
        }
        let Some((input_id, input)) = self
            .notify_name_input
            .as_ref()
            .filter(|(id, _)| id == &notify.id)
        else {
            return v_flex().flex_1().child("Preparing notify details");
        };
        let _ = input_id;
        let input = input.clone();
        let id = notify.id.clone();
        let editor_toggle = self.editor.clone();
        let id_toggle = id.clone();
        let editor_delete = self.editor.clone();
        let id_delete = id.clone();
        let editor_for_add = self.editor.clone();
        let id_for_add = id.clone();
        let add_field_button =
            move |button_id: &'static str, label: &'static str, value: NotifyValue| {
                let editor = editor_for_add.clone();
                let id = id_for_add.clone();
                Button::new(button_id)
                    .label(label)
                    .ghost()
                    .xsmall()
                    .on_click(move |_, _, cx| {
                        let id = id.clone();
                        let value = value.clone();
                        let _ = editor.update(cx, |e, cx| {
                            e.add_notify_field_value(&id, value);
                            cx.notify();
                        });
                    })
            };
        let target = match &notify.target {
            NotifyTarget::Self_ => "Self".to_string(),
            NotifyTarget::ClassVariable(_) => "Class variable".to_string(),
        };
        let target_editor = self
            .notify_target_input
            .as_ref()
            .filter(|(target_id, _)| target_id == &notify.id)
            .map(|(_, input)| input.clone());
        let time_input = self
            .notify_time_input
            .as_ref()
            .filter(|(time_id, _)| time_id == &notify.id)
            .map(|(_, input)| input.clone());
        let is_state = notify.end.is_some();
        let end_input = self
            .state_inputs
            .get(&format!("{}:end", notify.id))
            .map(|(input, _)| input.clone());
        let tick_input = self
            .state_inputs
            .get(&format!("{}:tick", notify.id))
            .map(|(input, _)| input.clone());
        let mut fields = v_flex().w_full().gap_1();
        for (index, field) in notify.fields.iter().enumerate() {
            let kind = match &field.value {
                NotifyValue::Bool(_) => "bool",
                NotifyValue::Int(_) => "int",
                NotifyValue::Float(_) => "float",
                NotifyValue::String(_) => "string",
            };
            let value = match &field.value {
                NotifyValue::Bool(value) => value.to_string(),
                NotifyValue::Int(value) => value.to_string(),
                NotifyValue::Float(value) => value.to_string(),
                NotifyValue::String(value) => value.clone(),
            };
            let name_input =
                self.notify_field_input(&notify.id, index, false, field.name.clone(), window, cx);
            let value_input = self.notify_field_input(&notify.id, index, true, value, window, cx);
            let editor_remove = self.editor.clone();
            let id_remove = id.clone();
            fields = fields.child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(Tag::secondary().small().child(kind))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(TextInput::new(&name_input).small()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(TextInput::new(&value_input).small()),
                    )
                    .child(
                        Button::new(SharedString::from(format!("notify-field-remove-{index}")))
                            .label("Remove")
                            .ghost()
                            .xsmall()
                            .on_click(move |_, _, cx| {
                                let id = id_remove.clone();
                                let _ = editor_remove.update(cx, |e, cx| {
                                    e.delete_notify_field(&id, index);
                                    cx.notify();
                                });
                            }),
                    ),
            );
        }
        v_flex()
            .w_full()
            .flex_1()
            .min_h_0()
            .gap_3()
            .p_3()
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Event name"),
                    )
                    .child(TextInput::new(&input).small()),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(if is_state { "Start" } else { "Time" }),
                    )
                    .when_some(time_input, |col, input| {
                        col.child(
                            NumberInput::new(&input).small().suffix(
                                div()
                                    .pr_1()
                                    .text_xs()
                                    .font_family(MONO)
                                    .text_color(theme.muted_foreground)
                                    .child("s"),
                            ),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(if is_state {
                                "Drag the bar to slide it, or its ends to resize."
                            } else {
                                "Drag its marker in the Notifies lane to move it."
                            }),
                    ),
            )
            .when(is_state, |col| {
                let seconds = |theme: &ui::Theme| {
                    div()
                        .pr_1()
                        .text_xs()
                        .font_family(MONO)
                        .text_color(theme.muted_foreground)
                        .child("s")
                };
                let length = notify.end.unwrap_or(notify.time) - notify.time;
                let editor_disable = self.editor.clone();
                let id_disable = id.clone();
                let disabled = notify.disabled;
                col.child(
                    v_flex()
                        .gap_1()
                        .child(div().text_xs().text_color(theme.muted_foreground).child("End"))
                        .when_some(end_input, |c, input| {
                            c.child(NumberInput::new(&input).small().suffix(seconds(&theme)))
                        })
                        .child(
                            div()
                                .text_xs()
                                .font_family(MONO)
                                .text_color(theme.muted_foreground)
                                .child(format!("Length {length:.3} s")),
                        ),
                )
                .child(
                    v_flex()
                        .gap_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("Tick interval (blank = every update)"),
                        )
                        .when_some(tick_input, |c, input| {
                            c.child(NumberInput::new(&input).small().suffix(seconds(&theme)))
                        }),
                )
                .child(
                    Button::new("notify-state-disabled")
                        .label(if disabled { "Disabled — click to enable" } else { "Enabled — click to disable" })
                        .ghost()
                        .small()
                        .tooltip("A disabled state stays in the asset but never dispatches events")
                        .on_click(move |_, _, cx| {
                            let id = id_disable.clone();
                            let _ = editor_disable.update(cx, |e, cx| {
                                e.set_notify_state_disabled(&id, !disabled);
                                cx.notify();
                            });
                        }),
                )
            })
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("Target"),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(
                                Button::new("notify-target-toggle")
                                    .label(target)
                                    .ghost()
                                    .small()
                                    .tooltip("Toggle between Self and a class variable target")
                                    .on_click(move |_, _, cx| {
                                        let id = id_toggle.clone();
                                        let _ = editor_toggle.update(cx, |e, cx| {
                                            e.toggle_notify_target(&id);
                                            cx.notify();
                                        });
                                    }),
                            )
                            .when_some(target_editor, |row, input| {
                                row.child(div().flex_1().child(TextInput::new(&input).small()))
                            }),
                    ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .items_center()
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .font_semibold()
                                    .text_color(theme.foreground)
                                    .child("Event fields"),
                            )
                            .child(add_field_button(
                                "notify-field-add-string",
                                "+str",
                                NotifyValue::String(String::new()),
                            ))
                            .child(add_field_button(
                                "notify-field-add-bool",
                                "+bool",
                                NotifyValue::Bool(false),
                            ))
                            .child(add_field_button(
                                "notify-field-add-int",
                                "+int",
                                NotifyValue::Int(0),
                            ))
                            .child(add_field_button(
                                "notify-field-add-float",
                                "+float",
                                NotifyValue::Float(0.0),
                            )),
                    )
                    .child(fields),
            )
            .child(
                div()
                    .mt_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(
                    "These values are serialized with the sequence and sent with the notify event.",
                ),
            )
            .child(
                Button::new("notify-delete")
                    .label(if is_state { "Delete state" } else { "Delete notify" })
                    .ghost()
                    .small()
                    .on_click(move |_, _, cx| {
                        let id = id_delete.clone();
                        let _ = editor_delete.update(cx, |e, cx| {
                            e.delete_notify(&id);
                            cx.notify();
                        });
                    }),
            )
    }

    /// A text input bound to one editor edit. `apply` receives the marker id
    /// and the text on every change; an input the user is not typing in is
    /// refreshed to `current` so snapping and drags show up.
    fn state_input(
        &mut self,
        key: String,
        current: String,
        window: &mut Window,
        cx: &mut Context<Self>,
        apply: impl Fn(&mut SkeletalAnimEditorPanel, &str, &str) + 'static,
    ) -> Entity<InputState> {
        if let Some((input, _)) = self.state_inputs.get(&key) {
            let input = input.clone();
            if !input.read(cx).focus_handle(cx).is_focused(window)
                && input.read(cx).value().as_ref() != current.as_str()
            {
                input.update(cx, |input, cx| input.set_value(current, window, cx));
            }
            return input;
        }
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
        let editor = self.editor.clone();
        let id = key.split(':').next().unwrap_or_default().to_string();
        let subscription = cx.subscribe_in(
            &input,
            window,
            move |_this, input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    if let Some(editor) = editor.upgrade() {
                        editor.update(cx, |e, cx| {
                            apply(e, &id, &text);
                            cx.notify();
                        });
                    }
                }
            },
        );
        self.state_inputs.insert(key, (input.clone(), subscription));
        input
    }

    fn notify_field_input(
        &mut self,
        notify_id: &str,
        index: usize,
        is_value: bool,
        initial: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let key = format!(
            "{notify_id}:{index}:{}",
            if is_value { "value" } else { "name" }
        );
        if let Some(input) = self.notify_field_inputs.get(&key) {
            return input.clone();
        }
        let input = cx.new(|cx| InputState::new(window, cx).default_value(initial));
        let editor = self.editor.clone();
        let id = notify_id.to_string();
        let subscription = cx.subscribe_in(
            &input,
            window,
            move |_this, input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = input.read(cx).value().to_string();
                    if let Some(editor) = editor.upgrade() {
                        editor.update(cx, |e, cx| {
                            if is_value {
                                e.set_notify_field_value(&id, index, value);
                            } else {
                                e.set_notify_field_name(&id, index, value);
                            }
                            cx.notify();
                        });
                    }
                }
            },
        );
        self.notify_field_inputs.insert(key.clone(), input.clone());
        self.notify_field_subscriptions.insert(key, subscription);
        input
    }
}

#[cfg(test)]
mod reset_interaction_tests {
    use super::BonePropertiesPanel;
    use crate::core::{assets, sample, Transform, Vec3};
    use crate::editor::hierarchy_edit::HierarchyCommand;
    use crate::editor::SkeletalAnimEditorPanel;
    use gpui::{AppContext, Modifiers, TestAppContext};

    #[gpui::test]
    fn rendered_reset_buttons_use_exact_defaults_and_do_not_collapse_sections(
        cx: &mut TestAppContext,
    ) {
        cx.update(ui::init);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Pulsar.toml"), "").unwrap();
        let path = dir.path().join("rig.skeleton");
        assets::save_skeleton_asset(
            &path,
            &assets::SkeletonAsset::new(sample::humanoid_skeleton()),
        )
        .unwrap();
        let mut editor_entity = None;
        let mut properties_entity = None;
        let (_, cx) = cx.add_window_view(|w, cx| {
            let editor = cx.new(|cx| SkeletalAnimEditorPanel::new_with_path(path, w, cx).unwrap());
            let properties = cx.new(|cx| BonePropertiesPanel::new(editor.downgrade(), cx));
            editor_entity = Some(editor);
            properties_entity = Some(properties.clone());
            ui::Root::new(properties.into(), w, cx)
        });
        let editor = editor_entity.unwrap();
        let properties = properties_entity.unwrap();
        editor.update(cx, |e, cx| {
            properties.update(cx, |p, cx| p.attach(e, &editor, cx))
        });
        let precise = Transform {
            translation: Vec3::new(0.0004, 2.0, 0.0),
            rotation: Vec3::new(0.0004, 2.0, 0.0),
            scale: Vec3::new(1.0004, 2.0, 1.0),
        };
        for socket in [false, true] {
            editor.update_in(cx, |e, w, cx| {
                e.select_bone(Some("root".into()), w, cx);
                if socket {
                    e.request_hierarchy_command(
                        HierarchyCommand::AddSocket {
                            bone: "root".into(),
                        },
                        w,
                        cx,
                    );
                }
                e.request_hierarchy_command(HierarchyCommand::Transform(precise.clone()), w, cx);
            });
            for selector in [
                "props-reset-Translation",
                "props-reset-Rotation",
                "props-reset-Scale",
            ] {
                for _ in 0..3 {
                    cx.run_until_parked();
                    cx.update(|w, cx| {
                        w.refresh();
                        w.draw(cx).clear()
                    });
                }
                let bounds = cx.debug_bounds(selector).unwrap();
                cx.simulate_mouse_move(bounds.center(), None, Modifiers::default());
                cx.simulate_mouse_down(
                    bounds.center(),
                    gpui::MouseButton::Left,
                    Modifiers::default(),
                );
                cx.update(|w, cx| {
                    w.refresh();
                    w.draw(cx).clear()
                });
                cx.simulate_mouse_up(
                    bounds.center(),
                    gpui::MouseButton::Left,
                    Modifiers::default(),
                );
                cx.run_until_parked();
            }
            editor.read_with(cx, |e, _| {
                assert_eq!(e.current_transform(), Transform::IDENTITY)
            });
            properties.read_with(cx, |p, _| assert!(p.collapsed.is_empty()));
            for _ in 0..3 {
                editor.update_in(cx, |e, w, cx| e.undo_hierarchy(false, w, cx));
            }
            editor.read_with(cx, |e, _| assert_eq!(e.current_transform(), precise));
            for _ in 0..3 {
                editor.update_in(cx, |e, w, cx| e.undo_hierarchy(true, w, cx));
            }
            editor.read_with(cx, |e, _| {
                assert_eq!(e.current_transform(), Transform::IDENTITY)
            });
        }
    }
}
