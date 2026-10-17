//! Screen 12, Model settings (ALK-US-0069, ALK-SP-0008.R5, ALK-SP-0010.R3): the per-model
//! overlay with the effective value and origin of every setting, the model card defaults,
//! a live memory estimate and the engine launch line.
//!
//! The rules live in [`crate::model_settings`] (GPUI-free, unit tested); this view draws the
//! form, forwards edits, asks the server for the estimate after a typing pause and saves the
//! overlay (`PUT /alkahest/v1/models/{name}/settings`). The overlay lives in Alkahest's data
//! dir, so read-only Ollama-store models are configured the same way without touching the
//! Ollama store.

use alkahest_client::Client;
use alkahest_client::LibraryModel;
use alkahest_client::admin::ModelSettingsDoc;
use serde_json::{Map, Value};

use crate::model_settings::{
    CONTEXT_STOPS, Debounce, ENGINES, ESTIMATE_DEBOUNCE, EstimateTone, EstimateView, FieldDisplay,
    KEEP_ALIVE_PRESETS, KV_TYPES, Origin, SettingsForm, context_position, context_stop_label,
};
use crate::ops::{self, ModelOp};
use crate::present::{format_tag, group_digits, is_read_only, kind_of};
use crate::theme::tokens::{Palette, radius, size, space, text};
use crate::tokio_bridge;
use crate::ui::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use crate::ui::{
    AnyElement, AppContext as _, Context, Div, Entity, EventEmitter, FontWeight,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString, Stateful,
    StatefulInteractiveElement, Styled, Task, Window, div, h_flex, px, relative, v_flex,
};
use crate::views::style::{TextStyleExt, Theme, c};
use crate::views::widgets::{
    ButtonKind, button, card, choice_chip, column_label, field_frame, field_label,
    format_tag as format_tag_widget, kind_badge, notice_line, segment, segmented,
};

/// Width of the side column (memory estimate, model card, launch line).
const SIDE_WIDTH: f32 = 400.0;

/// Text fields of the form, by overlay key (plus `parallel`, estimate only).
const TEXT_FIELDS: [&str; 12] = [
    "keep_alive",
    "context_length",
    "gpu_layers",
    "temperature",
    "top_p",
    "top_k",
    "min_p",
    "repeat_penalty",
    "presence_penalty",
    "max_tokens",
    "seed",
    "extra_args",
];

/// Settings keys the memory estimate depends on.
const ESTIMATE_KEYS: [&str; 6] = [
    "context_length",
    "kv_cache_type_k",
    "kv_cache_type_v",
    "flash_attention",
    "gpu_layers",
    "parallel",
];

/// What the settings screen asks of its host (the Library).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsEvent {
    /// Back to the Library.
    Close,
}

/// The template the model ships with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ModelTemplate {
    text: Option<String>,
    from_gguf: bool,
    model_file: Option<String>,
}

/// The estimate panel state.
#[derive(Debug, Clone, PartialEq)]
enum EstimateState {
    Idle,
    Loading,
    Ready(EstimateView),
    Failed(String),
}

/// The model settings view of one model.
pub struct ModelSettingsView {
    client: Client,
    model: LibraryModel,
    form: Option<SettingsForm>,
    load_error: Option<String>,
    template: ModelTemplate,
    custom_template: bool,
    inputs: Vec<(&'static str, Entity<InputState>)>,
    parallel: Entity<InputState>,
    template_input: Entity<TextareaState>,
    estimate: EstimateState,
    debounce: Debounce,
    estimate_task: Option<Task<()>>,
    saving: bool,
    loaded: bool,
    notice: Option<(bool, String)>,
}

impl std::fmt::Debug for ModelSettingsView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelSettingsView")
            .field("model", &self.model.name)
            .field("dirty", &self.form.as_ref().map(SettingsForm::dirty))
            .finish_non_exhaustive()
    }
}

impl EventEmitter<SettingsEvent> for ModelSettingsView {}

impl ModelSettingsView {
    /// Opens the settings of `model` and fetches them.
    pub fn new(
        client: Client,
        model: LibraryModel,
        loaded: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut inputs = Vec::new();
        for key in TEXT_FIELDS {
            let input = cx.new(|cx| InputState::new(window, cx));
            cx.subscribe_in(
                &input,
                window,
                move |this, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        let text = input.read(cx).value().to_string();
                        this.edit_text(key, &text, cx);
                    }
                },
            )
            .detach();
            inputs.push((key, input));
        }
        let parallel = cx.new(|cx| InputState::new(window, cx).default_value("1"));
        cx.subscribe_in(
            &parallel,
            window,
            |this, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    if let Some(form) = this.form.as_mut() {
                        form.parallel = text.trim().parse::<u32>().unwrap_or(1).clamp(1, 64);
                    }
                    this.schedule_estimate(cx);
                    cx.notify();
                }
            },
        )
        .detach();
        let template_input = cx.new(|cx| TextareaState::new(window, cx).rows(10));
        cx.subscribe_in(
            &template_input,
            window,
            |this, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) && this.custom_template {
                    let text = input.read(cx).value().to_string();
                    if let Some(form) = this.form.as_mut() {
                        form.set_text("template", &text);
                    }
                    cx.notify();
                }
            },
        )
        .detach();
        let mut view = Self {
            client,
            model,
            form: None,
            load_error: None,
            template: ModelTemplate::default(),
            custom_template: false,
            inputs,
            parallel,
            template_input,
            estimate: EstimateState::Idle,
            debounce: Debounce::default(),
            estimate_task: None,
            saving: false,
            loaded,
            notice: None,
        };
        view.fetch(window, cx);
        view
    }

    /// The model this screen edits.
    pub fn model_name(&self) -> &str {
        &self.model.name
    }

    /// Follows the live state: "Save & reload" is offered while the model is loaded.
    pub fn set_loaded(&mut self, loaded: bool, cx: &mut Context<Self>) {
        if self.loaded != loaded {
            self.loaded = loaded;
            cx.notify();
        }
    }

    /// Fetches the settings document and the model's template and file.
    fn fetch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let name = self.model.name.clone();
        let call = tokio_bridge::spawn(cx, async move {
            tokio::join!(client.model_settings(&name), client.model(&name))
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok((doc, detail)) = call.await else {
                return;
            };
            let _ = this.update_in(cx, |view, window, cx| {
                if let Ok(d) = detail {
                    view.template = ModelTemplate {
                        text: d
                            .extra
                            .get("template")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        from_gguf: d
                            .extra
                            .get("template_from_gguf")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        model_file: d.model_path.clone(),
                    };
                }
                match doc {
                    Ok(doc) => view.show_doc(doc, window, cx),
                    Err(e) => view.load_error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Shows a fresh settings document: form, inputs and template, then asks for an estimate.
    fn show_doc(&mut self, doc: ModelSettingsDoc, window: &mut Window, cx: &mut Context<Self>) {
        match self.form.as_mut() {
            Some(form) => form.reload(doc),
            None => self.form = Some(SettingsForm::new(doc)),
        }
        self.sync_inputs(window, cx);
        self.schedule_estimate(cx);
    }

    /// Puts the draft into every input: overlay values as text, effective values (and their
    /// origin in the caption) as placeholders.
    fn sync_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.form.as_ref() else {
            return;
        };
        let values: Vec<(Entity<InputState>, String, String)> = self
            .inputs
            .iter()
            .map(|(key, input)| {
                let d = form.display(key);
                let value = if d.overridden {
                    d.text.clone()
                } else {
                    String::new()
                };
                (input.clone(), value, d.text)
            })
            .collect();
        for (input, value, placeholder) in values {
            input.update(cx, |s, cx| {
                s.set_placeholder(placeholder, window, cx);
                s.set_value(value, window, cx);
            });
        }
        let template = form
            .draft()
            .get("template")
            .and_then(Value::as_str)
            .map(str::to_string);
        self.custom_template = template.is_some();
        let text = template
            .or_else(|| self.template.text.clone())
            .unwrap_or_default();
        self.template_input
            .update(cx, |s, cx| s.set_value(text, window, cx));
    }

    fn edit_text(&mut self, key: &'static str, text: &str, cx: &mut Context<Self>) {
        let Some(form) = self.form.as_mut() else {
            return;
        };
        let changed = form.set_text(key, text);
        if changed && ESTIMATE_KEYS.contains(&key) {
            self.schedule_estimate(cx);
        }
        cx.notify();
    }

    /// Sets (or clears, with `None`) a chip-chosen value and keeps its text input in step.
    fn choose(
        &mut self,
        key: &'static str,
        value: Option<Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(form) = self.form.as_mut() else {
            return;
        };
        let changed = match value {
            Some(v) => form.set_value(key, v),
            None => form.clear(key),
        };
        if !changed {
            return;
        }
        if let Some((_, input)) = self.inputs.iter().find(|(k, _)| *k == key) {
            let d = form.display(key);
            let text = if d.overridden {
                d.text.clone()
            } else {
                String::new()
            };
            input.update(cx, |s, cx| {
                s.set_placeholder(d.text, window, cx);
                s.set_value(text, window, cx);
            });
        }
        if ESTIMATE_KEYS.contains(&key) {
            self.schedule_estimate(cx);
        }
        cx.notify();
    }

    /// Asks for a memory estimate once edits pause; stale answers are dropped.
    fn schedule_estimate(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.form.as_ref() else {
            return;
        };
        if !self.model.format.eq_ignore_ascii_case("gguf") {
            self.estimate =
                EstimateState::Failed("Memory estimates are available for GGUF models.".into());
            return;
        }
        let generation = self.debounce.bump();
        let query = form.estimate_query();
        let client = self.client.clone();
        let name = self.model.name.clone();
        if self.estimate == EstimateState::Idle {
            self.estimate = EstimateState::Loading;
        }
        self.estimate_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ESTIMATE_DEBOUNCE).await;
            let still = this
                .read_with(cx, |view, _| view.debounce.is_current(generation))
                .unwrap_or(false);
            if !still {
                return;
            }
            let call = cx.update(|cx| {
                tokio_bridge::spawn(cx, async move {
                    let q: Vec<(&str, String)> = query;
                    client.estimate_memory(&name, &q).await
                })
            });
            let result = call.await;
            let _ = this.update(cx, |view, cx| {
                if !view.debounce.is_current(generation) {
                    return;
                }
                view.estimate = match result {
                    Ok(Ok(v)) => EstimateView::parse(&v).map_or_else(
                        || EstimateState::Failed("The server sent no estimate.".into()),
                        EstimateState::Ready,
                    ),
                    Ok(Err(e)) => EstimateState::Failed(e.to_string()),
                    Err(e) => EstimateState::Failed(format!("the request was cancelled: {e}")),
                };
                cx.notify();
            });
        }));
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.form.as_ref() else {
            return;
        };
        let overlay: Map<String, Value> = match form.to_save() {
            Ok(o) => o,
            Err(e) => {
                self.notice = Some((true, e));
                cx.notify();
                return;
            }
        };
        self.saving = true;
        self.notice = None;
        let reload = self.loaded;
        let client = self.client.clone();
        let name = self.model.name.clone();
        let call = tokio_bridge::spawn(cx, async move {
            let doc = client.put_model_settings(&name, &overlay).await?;
            let mut reload_error = None;
            if reload {
                // The overlay applies at load time: unload, then load with the new settings.
                for op in [ModelOp::Unload(name.clone()), ModelOp::Load(name.clone())] {
                    if let Err(e) = ops::run(&client, &op).await {
                        reload_error = Some(e.message);
                        break;
                    }
                }
            }
            Ok::<_, alkahest_client::ClientError>((doc, reload_error))
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |view, window, cx| {
                view.saving = false;
                match result {
                    Ok(Ok((doc, reload_error))) => {
                        view.show_doc(doc, window, cx);
                        view.notice = Some(match reload_error {
                            Some(e) => (true, format!("Saved, but the reload failed: {e}")),
                            None if reload => (false, "Saved and reloaded.".into()),
                            None => (false, "Saved. The settings apply on the next load.".into()),
                        });
                    }
                    Ok(Err(e)) => view.notice = Some((true, format!("Saving failed: {e}"))),
                    Err(e) => view.notice = Some((true, format!("Saving was cancelled: {e}"))),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// "Reset to card defaults": drops the overlay on the server.
    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let name = self.model.name.clone();
        let call = tokio_bridge::spawn(cx, async move { client.reset_model_settings(&name).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |view, window, cx| {
                match result {
                    Ok(Ok(doc)) => {
                        view.show_doc(doc, window, cx);
                        view.notice = Some((false, "Back to the model card defaults.".into()));
                    }
                    Ok(Err(e)) => view.notice = Some((true, format!("Reset failed: {e}"))),
                    Err(e) => view.notice = Some((true, format!("Reset was cancelled: {e}"))),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn apply_card(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(form) = self.form.as_mut()
            && form.apply_card() > 0
        {
            self.sync_inputs(window, cx);
            self.schedule_estimate(cx);
            cx.notify();
        }
    }

    fn set_custom_template(&mut self, custom: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.custom_template = custom;
        let Some(form) = self.form.as_mut() else {
            return;
        };
        if custom {
            let text = self.template.text.clone().unwrap_or_default();
            form.set_text("template", &text);
            self.template_input
                .update(cx, |s, cx| s.set_value(text, window, cx));
        } else {
            form.clear("template");
            let text = self.template.text.clone().unwrap_or_default();
            self.template_input
                .update(cx, |s, cx| s.set_value(text, window, cx));
        }
        cx.notify();
    }

    fn input(&self, key: &str) -> Option<&Entity<InputState>> {
        self.inputs.iter().find(|(k, _)| *k == key).map(|(_, i)| i)
    }

    // ----------------------------------------------------------------------------- drawing

    fn header(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let read_only = is_read_only(&self.model);
        let mut tags = h_flex()
            .gap(px(space::S2))
            .items_center()
            .child(kind_badge(kind_of(&self.model)))
            .child(format_tag_widget(&format_tag(&self.model), p));
        let source = if read_only {
            "ollama store \u{b7} read-only blob"
        } else {
            "alkahest store"
        };
        let mut source_tag = div()
            .px(px(space::S2))
            .py(px(3.0))
            .rounded(px(radius::SM))
            .border_1()
            .border_color(c(if read_only {
                p.line_dashed
            } else {
                p.line_strong
            }))
            .type_style(text::MONO)
            .text_size(px(text::CAPTION.size))
            .text_color(c(p.text_secondary))
            .child(source);
        if read_only {
            source_tag = source_tag.border_dashed();
        }
        tags = tags.child(source_tag);
        let dirty = self.form.as_ref().is_some_and(SettingsForm::dirty);
        let has_errors = self.form.as_ref().is_some_and(|f| !f.errors().is_empty());
        let save_label = if self.saving {
            "Saving\u{2026}"
        } else if self.loaded {
            "Save & reload"
        } else {
            "Save"
        };
        let save_disabled =
            self.saving || has_errors || self.form.is_none() || (!dirty && !self.loaded);
        let mut save = button(
            "settings-save",
            save_label,
            ButtonKind::Primary,
            size::CONTROL,
            save_disabled,
            p,
        );
        if !save_disabled {
            save = save.on_click(cx.listener(|this, _, window, cx| this.save(window, cx)));
        }
        let reset_disabled = self.form.is_none() || self.saving;
        let mut reset = div()
            .id("settings-reset")
            .px(px(space::S3))
            .type_style(text::SMALL)
            .font_weight(FontWeight::MEDIUM)
            .text_color(c(p.text_primary));
        if !reset_disabled {
            let hover = c(p.accent_text);
            reset = reset
                .cursor_pointer()
                .hover(move |s| s.text_color(hover))
                .on_click(cx.listener(|this, _, window, cx| this.reset(window, cx)));
        }
        let mut status = div().type_style(text::SMALL).text_color(c(p.text_muted));
        if let Some(form) = &self.form
            && form.dirty()
        {
            let n = form.changes();
            status = status.text_color(c(p.accent_text)).child(format!(
                "{n} unsaved change{}",
                if n == 1 { "" } else { "s" }
            ));
        }
        h_flex()
            .items_end()
            .gap(px(space::S4))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(space::S1))
                    .child(
                        h_flex()
                            .gap(px(space::S2))
                            .type_style(text::SMALL)
                            .text_color(c(p.text_muted))
                            .child(
                                div()
                                    .id("settings-breadcrumb-library")
                                    .cursor_pointer()
                                    .text_color(c(p.accent_text))
                                    .child("Library")
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(SettingsEvent::Close);
                                    })),
                            )
                            .child("/")
                            .child(
                                div()
                                    .type_style(text::MONO)
                                    .text_size(px(text::SMALL.size))
                                    .child(SharedString::from(self.model.name.clone())),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap(px(space::S3))
                            .items_center()
                            .child(
                                div()
                                    .type_style(text::H1)
                                    .child(SharedString::from(self.model.name.clone())),
                            )
                            .child(tags),
                    ),
            )
            .child(status)
            .child(reset.child("Reset to card defaults"))
            .child(save)
    }

    /// Label row of a field: label, origin caption, per-field reset when the overlay sets it.
    fn label_row(
        &self,
        key: &'static str,
        label: &'static str,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let d = self
            .form
            .as_ref()
            .map(|f| f.display(key))
            .unwrap_or(FieldDisplay {
                value: None,
                text: String::new(),
                origin: Origin::Automatic,
                overridden: false,
            });
        let mut row = h_flex()
            .gap(px(space::S2))
            .items_center()
            .child(field_label(label, p))
            .child(origin_caption(&d, p));
        if d.overridden {
            row = row.child(
                div()
                    .id(SharedString::from(format!("reset-{key}")))
                    .ml_auto()
                    .type_style(text::CAPTION)
                    .text_color(c(p.text_muted))
                    .cursor_pointer()
                    .child("reset")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose(key, None, window, cx);
                    })),
            );
        }
        row
    }

    fn text_field(
        &self,
        key: &'static str,
        label: &'static str,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let error = self
            .form
            .as_ref()
            .and_then(|f| f.errors().get(key).cloned());
        let mut col = v_flex()
            .gap(px(6.0))
            .min_w_0()
            .child(self.label_row(key, label, p, cx));
        if let Some(input) = self.input(key) {
            col = col.child(
                field_frame(error.is_some(), p).child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .type_style(text::MONO)
                        .child(Input::new(input).appearance(false)),
                ),
            );
        }
        if let Some(e) = error {
            col = col.child(
                div()
                    .type_style(text::CAPTION)
                    .text_color(c(p.danger_text))
                    .child(e),
            );
        }
        col
    }

    /// A row of choice chips for `key`: `Automatic` (clears it) then each option.
    fn chips(
        &self,
        key: &'static str,
        options: &[(&'static str, Value)],
        auto_label: &'static str,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let current = self.form.as_ref().and_then(|f| f.draft().get(key).cloned());
        let mut row = h_flex().flex_wrap().gap(px(6.0));
        row = row.child(
            choice_chip(
                SharedString::from(format!("{key}-auto")),
                auto_label,
                current.is_none(),
                false,
                p,
            )
            .on_click(cx.listener(move |this, _, window, cx| this.choose(key, None, window, cx))),
        );
        for (i, (label, value)) in options.iter().enumerate() {
            let on = current.as_ref() == Some(value);
            let v = value.clone();
            row = row.child(
                choice_chip(
                    SharedString::from(format!("{key}-{i}")),
                    *label,
                    on,
                    true,
                    p,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.choose(key, Some(v.clone()), window, cx);
                })),
            );
        }
        row
    }

    fn tri_state(
        &self,
        key: &'static str,
        label: &'static str,
        hint: &'static str,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let current = self
            .form
            .as_ref()
            .and_then(|f| f.draft().get(key).and_then(Value::as_bool));
        let mut segs = Vec::new();
        for (i, (l, v)) in [("auto", None), ("on", Some(true)), ("off", Some(false))]
            .into_iter()
            .enumerate()
        {
            segs.push(
                segment(
                    SharedString::from(format!("{key}-tri-{i}")),
                    l,
                    current == v,
                    p,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.choose(key, v.map(Value::Bool), window, cx);
                })),
            );
        }
        h_flex()
            .gap(px(space::S3))
            .items_center()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.0))
                    .child(self.label_row(key, label, p, cx))
                    .child(
                        div()
                            .type_style(text::CAPTION)
                            .text_color(c(p.text_muted))
                            .child(hint),
                    ),
            )
            .child(segmented(segs, p))
    }

    fn thinking_row(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let draft = self.form.as_ref().and_then(|f| {
            f.draft()
                .get("chat_template_kwargs")
                .and_then(|k| k["enable_thinking"].as_bool())
        });
        let effective = self.form.as_ref().and_then(SettingsForm::thinking);
        let mut segs = Vec::new();
        for (i, (l, v)) in [("auto", None), ("on", Some(true)), ("off", Some(false))]
            .into_iter()
            .enumerate()
        {
            segs.push(
                segment(
                    SharedString::from(format!("thinking-tri-{i}")),
                    l,
                    draft == v,
                    p,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(form) = this.form.as_mut()
                        && form.set_thinking(v)
                    {
                        cx.notify();
                    }
                })),
            );
        }
        let hint = match (draft, effective) {
            (None, Some(true)) => "enable_thinking in template kwargs \u{b7} card: on",
            (None, Some(false)) => "enable_thinking in template kwargs \u{b7} card: off",
            _ => "enable_thinking in template kwargs",
        };
        h_flex()
            .gap(px(space::S3))
            .items_center()
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.0))
                    .child(field_label("Thinking", p))
                    .child(
                        div()
                            .type_style(text::CAPTION)
                            .text_color(c(p.text_muted))
                            .child(hint),
                    ),
            )
            .child(segmented(segs, p))
    }

    fn context_slider(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let form = self.form.as_ref();
        let ctx = form.and_then(SettingsForm::context_length);
        let card = form
            .and_then(|f| f.card_value("context_length"))
            .and_then(Value::as_u64);
        // No layer sets a context: the thumb shows the server default the estimate used.
        let estimated = match &self.estimate {
            EstimateState::Ready(e) if e.context_length > 0 => Some(e.context_length),
            _ => None,
        };
        let shown = ctx.or(estimated);
        let pos = shown.map_or(0.0, context_position);
        let mut track = div().relative().w_full().h(px(size::SLIDER_THUMB)).child(
            div()
                .absolute()
                .top(px((size::SLIDER_THUMB - size::SLIDER_TRACK) / 2.0))
                .left(px(0.0))
                .w_full()
                .h(px(size::SLIDER_TRACK))
                .rounded(px(radius::FULL))
                .bg(c(p.muted_subtle))
                .child(
                    div()
                        .h_full()
                        .w(relative(pos))
                        .rounded(px(radius::FULL))
                        .bg(c(p.accent_default)),
                ),
        );
        if let Some(cv) = card {
            track = track.child(
                div()
                    .absolute()
                    .top(px(0.0))
                    .left(relative(context_position(cv)))
                    .w(px(2.0))
                    .h(px(size::SLIDER_THUMB))
                    .bg(c(p.accent_text)),
            );
        }
        if shown.is_some() {
            track = track.child(
                div()
                    .absolute()
                    .top(px(0.0))
                    .left(relative(pos))
                    .ml(px(-size::SLIDER_THUMB / 2.0))
                    .size(px(size::SLIDER_THUMB))
                    .rounded_full()
                    .bg(c(p.accent_default))
                    .border_2()
                    .border_color(c(p.bg_raised)),
            );
        }
        let mut stops = h_flex().justify_between();
        for (i, stop) in CONTEXT_STOPS.into_iter().enumerate() {
            let on = ctx == Some(stop);
            stops = stops.child(
                div()
                    .id(SharedString::from(format!("ctx-stop-{i}")))
                    .px(px(space::S1))
                    .rounded(px(radius::SM))
                    .type_style(text::MONO)
                    .text_size(px(text::CAPTION.size))
                    .text_color(c(if on { p.accent_text } else { p.text_muted }))
                    .cursor_pointer()
                    .child(context_stop_label(stop))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose("context_length", Some(Value::from(stop)), window, cx);
                    })),
            );
        }
        let mut title = h_flex().gap(px(space::S2)).items_center().child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child("Context window"),
        );
        if let Some(form) = form {
            title = title.child(origin_caption(&form.display("context_length"), p));
        }
        title = title.child(div().flex_1());
        if let Some(cv) = card {
            title = title.child(
                div()
                    .type_style(text::SMALL)
                    .text_color(c(p.accent_text))
                    .child(format!("card: {}", group_digits(cv))),
            );
        }
        let input = self.input("context_length").map(|input| {
            field_frame(
                form.is_some_and(|f| f.errors().contains_key("context_length")),
                p,
            )
            .w(px(120.0))
            .h(px(size::CONTROL_SMALL))
            .child(
                div()
                    .flex_1()
                    .type_style(text::MONO)
                    .child(Input::new(input).appearance(false)),
            )
        });
        let mut col = v_flex().gap(px(space::S2)).child(title.children(input));
        col = col.child(track).child(stops);
        if let Some(e) = form.and_then(|f| f.errors().get("context_length").cloned()) {
            col = col.child(
                div()
                    .type_style(text::CAPTION)
                    .text_color(c(p.danger_text))
                    .child(e),
            );
        }
        col
    }

    fn runtime_card(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let engines: Vec<(&'static str, Value)> = ENGINES
            .iter()
            .map(|e| (*e, Value::String((*e).to_string())))
            .collect();
        let keep: Vec<(&'static str, Value)> = KEEP_ALIVE_PRESETS
            .iter()
            .map(|(l, v)| (*l, Value::String((*v).to_string())))
            .collect();
        let kv: Vec<(&'static str, Value)> = KV_TYPES
            .iter()
            .map(|t| (*t, Value::String((*t).to_string())))
            .collect();
        let parallel = v_flex()
            .gap(px(6.0))
            .min_w_0()
            .child(
                h_flex()
                    .gap(px(space::S2))
                    .child(field_label("Parallel slots", p))
                    .child(
                        div()
                            .type_style(text::CAPTION)
                            .text_color(c(p.text_muted))
                            .child("estimate only \u{b7} server-wide setting"),
                    ),
            )
            .child(
                field_frame(false, p).child(
                    div()
                        .flex_1()
                        .type_style(text::MONO)
                        .child(Input::new(&self.parallel).appearance(false)),
                ),
            );
        card(p)
            .gap(px(space::S4))
            .child(div().type_style(text::TITLE).child("Runtime"))
            .child(
                v_flex()
                    .gap(px(6.0))
                    .child(self.label_row("engine", "Engine", p, cx))
                    .child(self.chips("engine", &engines, "automatic", p, cx)),
            )
            .child(
                v_flex()
                    .gap(px(6.0))
                    .child(self.label_row("keep_alive", "Keep alive after last request", p, cx))
                    .child(
                        h_flex()
                            .gap(px(space::S3))
                            .items_center()
                            .child(self.chips("keep_alive", &keep, "server default", p, cx))
                            .child(self.input("keep_alive").map_or_else(div, |input| {
                                field_frame(
                                    self.form
                                        .as_ref()
                                        .is_some_and(|f| f.errors().contains_key("keep_alive")),
                                    p,
                                )
                                .w(px(110.0))
                                .h(px(size::CONTROL_SMALL - 4.0))
                                .child(
                                    div()
                                        .flex_1()
                                        .type_style(text::MONO)
                                        .child(Input::new(input).appearance(false)),
                                )
                            })),
                    ),
            )
            .child(self.context_slider(p, cx))
            .child(
                h_flex()
                    .gap(px(space::S4))
                    .items_start()
                    .child(
                        v_flex()
                            .flex_1()
                            .gap(px(6.0))
                            .child(self.label_row("kv_cache_type_k", "KV cache K", p, cx))
                            .child(self.chips("kv_cache_type_k", &kv, "default", p, cx)),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .gap(px(6.0))
                            .child(self.label_row("kv_cache_type_v", "KV cache V", p, cx))
                            .child(self.chips("kv_cache_type_v", &kv, "default", p, cx)),
                    ),
            )
            .child(
                h_flex()
                    .gap(px(space::S4))
                    .child(
                        div()
                            .flex_1()
                            .child(self.text_field("gpu_layers", "GPU layers", p, cx)),
                    )
                    .child(div().flex_1().child(parallel)),
            )
            .child(self.tri_state(
                "flash_attention",
                "Flash attention",
                "smaller KV cache footprint, needed for quantised V cache",
                p,
                cx,
            ))
            .child(self.tri_state(
                "tools_enabled",
                "Tools / function calling",
                "auto follows the template's tool support",
                p,
                cx,
            ))
            .child(self.thinking_row(p, cx))
    }

    fn sampling_card(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let mut grid = div().flex().flex_wrap().gap_y(px(space::S3));
        for (key, label) in [
            ("temperature", "temperature"),
            ("top_p", "top_p"),
            ("top_k", "top_k"),
            ("min_p", "min_p"),
            ("repeat_penalty", "repeat_penalty"),
            ("presence_penalty", "presence_penalty"),
            ("max_tokens", "max_tokens"),
            ("seed", "seed"),
        ] {
            grid = grid.child(
                div()
                    .w(relative(0.25))
                    .pr(px(space::S3))
                    .child(self.text_field(key, label, p, cx)),
            );
        }
        card(p)
            .gap(px(space::S4))
            .child(div().type_style(text::TITLE).child("Sampling defaults"))
            .child(grid)
    }

    fn template_card(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let model_label = if self.template.from_gguf {
            "From GGUF (jinja)"
        } else {
            "Ollama TEMPLATE"
        };
        let segs = vec![
            segment("template-model", model_label, !self.custom_template, p).on_click(
                cx.listener(|this, _, window, cx| this.set_custom_template(false, window, cx)),
            ),
            segment("template-custom", "Custom", self.custom_template, p).on_click(
                cx.listener(|this, _, window, cx| this.set_custom_template(true, window, cx)),
            ),
        ];
        let body: AnyElement = if self.custom_template {
            div()
                .rounded(px(radius::LG))
                .border_1()
                .border_color(c(p.line_strong))
                .bg(c(p.bg_code))
                .p(px(space::S2))
                .type_style(text::CODE)
                .child(Textarea::new(&self.template_input).appearance(false))
                .into_any_element()
        } else {
            let text = self.template.text.clone().unwrap_or_else(|| {
                "The model ships no template; the engine default applies.".into()
            });
            let mut block = v_flex()
                .id("template-code")
                .max_h(px(260.0))
                .overflow_y_scroll()
                .p(px(14.0))
                .rounded(px(radius::LG))
                .bg(c(p.bg_code))
                .type_style(text::CODE)
                .text_color(c(p.text_secondary));
            for line in text.lines().take(400) {
                block = block.child(
                    div()
                        .min_h(px(text::CODE.size * text::CODE.line_height))
                        .child(line.to_string()),
                );
            }
            block.into_any_element()
        };
        card(p)
            .gap(px(space::S4))
            .child(
                h_flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .type_style(text::TITLE)
                            .child("Chat template"),
                    )
                    .child(segmented(segs, p)),
            )
            .child(body)
            .child(self.text_field("extra_args", "Extra engine arguments", p, cx))
    }

    fn memory_card(&self, p: &Palette) -> Div {
        let mut el = card(p).child(div().type_style(text::TITLE).child("Memory estimate"));
        match &self.estimate {
            EstimateState::Idle | EstimateState::Loading => {
                el = el.child(
                    div()
                        .type_style(text::SMALL)
                        .text_color(c(p.text_muted))
                        .child("Estimating\u{2026}"),
                );
            }
            EstimateState::Failed(e) => {
                el = el.child(
                    div()
                        .type_style(text::SMALL)
                        .text_color(c(p.text_muted))
                        .child(SharedString::from(e.clone())),
                );
            }
            EstimateState::Ready(est) => {
                let (num, unit) = est.headline();
                let [w, k, cmp] = est.segments();
                let (tone_fg, _) = match est.verdict().0 {
                    EstimateTone::Ok => (p.ok_text, p.ok_subtle),
                    EstimateTone::Warning => (p.accent_text, p.accent_subtle),
                    EstimateTone::Danger => (p.danger_text, p.danger_subtle),
                };
                let colors = [p.accent_default, p.ok_default, p.info_default];
                let bar = h_flex()
                    .w_full()
                    .h(px(size::METER))
                    .rounded(px(radius::FULL))
                    .bg(c(p.muted_subtle))
                    .overflow_hidden()
                    .child(div().h_full().w(relative(w)).bg(c(colors[0])))
                    .child(div().h_full().w(relative(k)).bg(c(colors[1])))
                    .child(div().h_full().w(relative(cmp)).bg(c(colors[2])));
                let mut legend = v_flex().gap(px(6.0));
                for (i, (label, value)) in est.legend().into_iter().enumerate() {
                    let color = colors[i.min(2)];
                    legend = legend.child(
                        h_flex()
                            .gap(px(space::S2))
                            .items_center()
                            .type_style(text::SMALL)
                            .child(div().size(px(8.0)).bg(c(color)))
                            .child(div().flex_1().min_w_0().truncate().child(label))
                            .child(
                                div()
                                    .type_style(text::MONO)
                                    .text_size(px(text::SMALL.size))
                                    .child(value),
                            ),
                    );
                }
                el = el
                    .child(
                        h_flex()
                            .gap(px(space::S2))
                            .items_end()
                            .child(div().type_style(text::MONO_LARGE).child(num))
                            .child(
                                div()
                                    .pb(px(4.0))
                                    .type_style(text::SMALL)
                                    .text_color(c(p.text_secondary))
                                    .child(unit),
                            ),
                    )
                    .child(bar)
                    .child(legend)
                    .child(
                        div()
                            .type_style(text::SMALL)
                            .text_color(c(tone_fg))
                            .child(est.verdict().1),
                    );
            }
        }
        el
    }

    fn card_says(&self, p: &Palette, cx: &mut Context<Self>) -> Option<Div> {
        let form = self.form.as_ref()?;
        let rec = form.doc().recommended.as_ref()?;
        let source = rec["sources"]
            .as_array()
            .map(|s| {
                s.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let diffs = form.card_diffs();
        let mut rows = v_flex().gap(px(6.0));
        for key in [
            "temperature",
            "top_p",
            "top_k",
            "min_p",
            "presence_penalty",
            "repeat_penalty",
            "context_length",
            "kv_cache_type_k",
            "kv_cache_type_v",
            "max_tokens",
        ] {
            let Some(cv) = form.card_value(key) else {
                continue;
            };
            let spec = crate::model_settings::field(key)?;
            let card_text = crate::model_settings::value_text(spec, cv);
            let diff = diffs.iter().find(|d| d.key == key);
            let (value, color) = match diff {
                Some(d) => (
                    format!("{card_text} \u{b7} you: {}", d.current),
                    p.accent_text,
                ),
                None => (format!("{card_text} \u{2713}"), p.text_primary),
            };
            rows = rows.child(
                h_flex()
                    .type_style(text::SMALL)
                    .child(
                        div()
                            .flex_1()
                            .text_color(c(p.text_secondary))
                            .child(spec.label),
                    )
                    .child(
                        div()
                            .type_style(text::MONO)
                            .text_size(px(text::SMALL.size))
                            .text_color(c(color))
                            .child(value),
                    ),
            );
        }
        let n = diffs.len();
        let mut el = card(p)
            .child(
                h_flex()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .type_style(text::TITLE)
                            .child("Model card says"),
                    )
                    .child(
                        div()
                            .type_style(text::MONO)
                            .text_size(px(text::CAPTION.size))
                            .text_color(c(p.text_muted))
                            .child(source),
                    ),
            )
            .child(rows);
        if n > 0 {
            el = el.child(
                h_flex().child(
                    button(
                        "settings-apply-card",
                        format!("Apply {n} difference{}", if n == 1 { "" } else { "s" }),
                        ButtonKind::Secondary,
                        size::CONTROL_SMALL,
                        false,
                        p,
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.apply_card(window, cx))),
                ),
            );
        }
        Some(el)
    }

    fn launch_card(&self, p: &Palette) -> Option<Div> {
        let form = self.form.as_ref()?;
        let file = self.template.model_file.as_deref().map(short_path);
        Some(
            card(p)
                .child(column_label("Launch line", p))
                .child(
                    div()
                        .type_style(text::MONO)
                        .text_size(px(text::CAPTION.size))
                        .text_color(c(p.text_secondary))
                        .child(form.launch_line(file.as_deref())),
                )
                .child(
                    div()
                        .type_style(text::CAPTION)
                        .text_color(c(p.text_muted))
                        .child(format!("Saved to {}", form.path())),
                ),
        )
    }
}

impl Render for ModelSettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Theme::get(cx).palette;
        let mut root = v_flex()
            .size_full()
            .gap(px(space::S5))
            .child(self.header(p, cx));
        if let Some((danger, text)) = &self.notice {
            root = root.child(notice_line(text.clone(), *danger, p));
        }
        if let Some(e) = &self.load_error {
            return root.child(notice_line(
                format!("The settings could not be loaded: {e}"),
                true,
                p,
            ));
        }
        if self.form.is_none() {
            return root.child(
                div()
                    .type_style(text::SMALL)
                    .text_color(c(p.text_muted))
                    .child("Loading settings\u{2026}"),
            );
        }
        let main: Stateful<Div> = v_flex()
            .id("settings-main")
            .flex_1()
            .min_w_0()
            .h_full()
            .gap(px(space::S5))
            .overflow_y_scroll()
            .child(self.runtime_card(p, cx))
            .child(self.sampling_card(p, cx))
            .child(self.template_card(p, cx));
        let mut side = v_flex()
            .id("settings-side")
            .w(px(SIDE_WIDTH))
            .flex_shrink_0()
            .h_full()
            .gap(px(space::S5))
            .overflow_y_scroll()
            .child(self.memory_card(p));
        if let Some(c) = self.card_says(p, cx) {
            side = side.child(c);
        }
        if let Some(c) = self.launch_card(p) {
            side = side.child(c);
        }
        root.child(
            h_flex()
                .flex_1()
                .min_h_0()
                .gap(px(space::S5))
                .items_start()
                .child(main)
                .child(side),
        )
    }
}

/// The caption after a field label: effective value origin (`recommended`, `edited`...).
fn origin_caption(d: &FieldDisplay, p: &Palette) -> Div {
    let color = match d.origin {
        Origin::Edited => p.accent_text,
        Origin::Model => p.text_secondary,
        Origin::Recommended => p.ok_text,
        _ => p.text_muted,
    };
    let text = match d.origin {
        // An edit that clears a saved value shows what will apply after saving.
        Origin::Edited if !d.overridden => format!("edited \u{b7} {}", d.text),
        o => o.label().to_string(),
    };
    div()
        .type_style(text::CAPTION)
        .text_color(c(color))
        .child(text)
}

/// The last path component (`…/blobs/sha256-a3de86…`), shortened for the launch line.
fn short_path(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    if name.chars().count() > 18 {
        let head: String = name.chars().take(17).collect();
        format!("{head}\u{2026}")
    } else {
        name.to_string()
    }
}

// GUI tests need gpui-kit's `test-support` (feature `gui-test`, the desktop CI job).
#[cfg(all(test, feature = "gui-test"))]
#[allow(clippy::unwrap_used)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::theme::ThemePreference;
    use crate::ui::WindowAppearance;
    use crate::ui::testing::{TestAppContext, gpui_test};

    fn setup(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        cx.update(|cx| {
            crate::ui::init(cx);
            Theme::install(cx, ThemePreference::Dark, WindowAppearance::Dark);
            tokio_bridge::init(cx).unwrap();
        });
    }

    fn dead_client() -> Client {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        Client::for_bind("127.0.0.1", port).unwrap()
    }

    #[gpui_test]
    fn the_form_renders_edits_and_drops_card_overrides(cx: &mut TestAppContext) {
        setup(cx);
        let model = LibraryModel {
            name: "qwen3:8b".into(),
            format: "gguf".into(),
            source: Some("ollama".into()),
            ..LibraryModel::default()
        };
        let (view, cx) = cx.add_window_view(|window, cx| {
            ModelSettingsView::new(dead_client(), model, false, window, cx)
        });
        cx.run_until_parked();
        let doc = ModelSettingsDoc {
            model: "qwen3:8b".into(),
            installed: true,
            overlay: json!({"temperature": 1.0}).as_object().cloned().unwrap(),
            recommended: Some(json!({"temperature": 0.6, "context_length": 32768})),
            effective: json!({"temperature": 1.0, "context_length": 32768})
                .as_object()
                .cloned()
                .unwrap(),
            origins: json!({"temperature": "model", "context_length": "recommended"})
                .as_object()
                .cloned()
                .unwrap(),
            ..ModelSettingsDoc::default()
        };
        view.update_in(cx, |v, window, cx| {
            v.load_error = None;
            v.show_doc(doc, window, cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |v, cx| {
            let f = v.form.as_ref().unwrap();
            assert_eq!(f.card_diffs().len(), 1);
            assert_eq!(
                v.input("temperature").unwrap().read(cx).value().as_ref(),
                "1.0"
            );
        });
        // Applying the card drops the temperature override; doubling the context through a
        // slider stop then updates the draft.
        view.update_in(cx, |v, window, cx| {
            v.apply_card(window, cx);
            v.choose("context_length", Some(Value::from(65_536_u64)), window, cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |v, cx| {
            let f = v.form.as_ref().unwrap();
            assert_eq!(f.context_length(), Some(65_536));
            assert!(f.card_diffs().iter().all(|d| d.key != "temperature"));
            assert_eq!(
                v.input("temperature").unwrap().read(cx).value().as_ref(),
                ""
            );
            assert!(f.dirty());
        });
    }
}
