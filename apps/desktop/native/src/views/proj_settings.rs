//! Project settings (screen 9): workflow form + advanced YAML + model routes.
//!
//! Parity source: `features/settings/SettingsPage.tsx`,
//! `WorkflowSettings.tsx`, `ModelSelection.tsx` (full port),
//! `SettingsSection.tsx`.
//! - Models section: tier map display + per-operation override Selects from
//!   `caps.operations` (raw `/capabilities` doc; the typed `Client` drops
//!   that field). `tier:<id>` / model-id values mirror `ModelSelection`;
//!   edits re-derive the YAML draft so restore/validate/save round-trip.
//!   "Manage models in global Settings" is a text note (cross-screen nav
//!   lands later).
use gpui_kit::component::button::Button;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Disableable, IndexPath, StyledExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::{
    client::{block_on, is_project_busy, Client, ProjectConfigOut},
    theme,
};

/// Tier ids in display order — mirrors `tierNames` in `ModelSelection.tsx`.
const TIERS: &[(&str, &str)] = &[
    ("strong", "Quality tier"),
    ("cheap", "Economy tier"),
    ("fast", "Fast tier"),
];

fn section(value: &serde_json::Value, key: &str) -> serde_json::Value {
    value.get(key).cloned().unwrap_or(serde_json::Value::Null)
}

fn get_str(effective: &serde_json::Value, group: &str, key: &str, default: &str) -> String {
    effective
        .get(group)
        .and_then(|g| g.get(key))
        .and_then(|v| v.as_str())
        .unwrap_or(default)
        .to_string()
}

fn get_i64(effective: &serde_json::Value, group: &str, key: &str, default: i64) -> i64 {
    effective
        .get(group)
        .and_then(|g| g.get(key))
        .and_then(|v| v.as_i64())
        .unwrap_or(default)
}

fn get_bool(effective: &serde_json::Value, group: &str, key: &str) -> bool {
    effective
        .get(group)
        .and_then(|g| g.get(key))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Mirror of `workflowField`: set `[group][key]` on the effective object.
fn workflow_field(
    effective: &serde_json::Value,
    group: &str,
    key: &str,
    value: serde_json::Value,
) -> serde_json::Value {
    let mut next = effective.clone();
    if !next.is_object() {
        next = serde_json::Value::Object(Default::default());
    }
    let mut group_obj = next
        .get(group)
        .cloned()
        .filter(|v| v.is_object())
        .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
    if let Some(map) = group_obj.as_object_mut() {
        map.insert(key.to_string(), value);
    }
    if let Some(map) = next.as_object_mut() {
        map.insert(group.to_string(), group_obj);
    }
    next
}

/// Mirror of the React `onChange` in `ModelSelection.tsx`: set
/// `llm.routes[op]` to `{model, fallbacks}` / `{tier, fallbacks}`, or delete
/// the override when it matches the default tier with no fallbacks.
fn operation_route(
    effective: &serde_json::Value,
    op: &str,
    value: &str,
    default_tier: &str,
) -> serde_json::Value {
    let mut next = effective.clone();
    if !next.is_object() {
        next = serde_json::Value::Object(Default::default());
    }
    let mut llm = next
        .get("llm")
        .cloned()
        .filter(|v| v.is_object())
        .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
    let mut routes = llm
        .get("routes")
        .cloned()
        .filter(|v| v.is_object())
        .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
    let existing_fallbacks = routes
        .get(op)
        .and_then(|r| r.get("fallbacks"))
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
    let has_fallbacks = existing_fallbacks
        .as_array()
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    let is_default_tier =
        value == format!("tier:{default_tier}") && !has_fallbacks;
    if is_default_tier {
        if let Some(map) = routes.as_object_mut() {
            map.remove(op);
        }
    } else {
        let mut entry = serde_json::Map::new();
        if let Some(tier) = value.strip_prefix("tier:") {
            entry.insert("tier".to_string(), serde_json::Value::from(tier));
        } else {
            entry.insert("model".to_string(), serde_json::Value::from(value));
        }
        entry.insert("fallbacks".to_string(), existing_fallbacks);
        if let Some(map) = routes.as_object_mut() {
            map.insert(op.to_string(), serde_json::Value::Object(entry));
        }
    }
    if let Some(map) = llm.as_object_mut() {
        map.insert("routes".to_string(), routes);
    }
    if let Some(map) = next.as_object_mut() {
        map.insert("llm".to_string(), llm);
    }
    next
}

pub struct ProjSettingsView {
    client: Option<Client>,
    pid: String,
    loading: bool,
    loaded: bool,
    error: Option<String>,
    notice: Option<String>,
    project_status: String,
    project_fmt: String,
    editable: bool,
    effective: serde_json::Value,
    registered_models: serde_json::Value,
    yaml: Entity<TextareaState>,
    pending_yaml: Option<String>,
    yaml_dirty: bool,
    batch_tokens: Entity<InputState>,
    segment_tokens: Entity<InputState>,
    review_concurrency: Entity<InputState>,
    pending_ints: [Option<String>; 3],
    typing_int: Option<usize>,
    pdf_backend: String,
    pdf_select: Option<Entity<SelectState<Vec<String>>>>,
    /// `caps.operations` (id + default tier) for per-operation overrides —
    /// mirrors the `operations` prop of `ModelSelection.tsx`. Sourced from
    /// the raw `/capabilities` document (`Client::capabilities` only keeps
    /// languages + formats).
    operations: Vec<OperationCaps>,
    /// Per-operation Select entities, keyed by operation id. The selected
    /// value is `tier:<id>` or a registered model id; `None` clears the
    /// override (mirrors the React `delete next[id]` path).
    op_selects: std::collections::HashMap<String, Entity<SelectState<Vec<String>>>>,
    validated_routes: Option<serde_json::Value>,
    saved_routes: Option<Vec<serde_json::Value>>,
    routes_error: Option<String>,
    check_result: Option<serde_json::Value>,
    check_error: Option<String>,
    checking: bool,
    op: Option<&'static str>,
    /// Whether this screen is currently shown; the 3s busy-poll only
    /// re-arms while visible (set by the App shell on navigation).
    visible: bool,
}

/// One `caps.operations[]` entry: overrideable operation + default tier.
#[derive(Debug, Clone)]
struct OperationCaps {
    id: String,
    tier: String,
}

impl ProjSettingsView {
    pub fn new(pid: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let yaml = cx.new(|cx| TextareaState::new(window, cx));
        cx.subscribe(&yaml, |this: &mut ProjSettingsView, _, _: &InputEvent, cx| {
            this.yaml_dirty = true;
            cx.notify();
        })
        .detach();
        let batch_tokens = cx.new(|cx| InputState::new(window, cx).placeholder("1800"));
        let segment_tokens = cx.new(|cx| InputState::new(window, cx).placeholder("1200"));
        let review_concurrency = cx.new(|cx| InputState::new(window, cx).placeholder("4"));
        for (entity, idx) in [
            (batch_tokens.clone(), 0usize),
            (segment_tokens.clone(), 1usize),
            (review_concurrency.clone(), 2usize),
        ] {
            cx.subscribe(
                &entity,
                move |this: &mut ProjSettingsView, _, _: &InputEvent, cx| {
                    this.on_int_changed(idx, cx);
                },
            )
            .detach();
        }
        Self {
            client: None,
            pid,
            loading: true,
            loaded: false,
            error: None,
            notice: None,
            project_status: String::new(),
            project_fmt: String::new(),
            editable: true,
            effective: serde_json::Value::Object(Default::default()),
            registered_models: serde_json::Value::Object(Default::default()),
            yaml,
            pending_yaml: None,
            yaml_dirty: false,
            batch_tokens,
            segment_tokens,
            review_concurrency,
            pending_ints: [None, None, None],
            typing_int: None,
            pdf_backend: "mineru".into(),
            pdf_select: None,
            operations: Vec::new(),
            op_selects: std::collections::HashMap::new(),
            validated_routes: None,
            saved_routes: None,
            routes_error: None,
            check_result: None,
            check_error: None,
            checking: false,
            op: None,
            visible: true,
        }
    }

    /// Called by the App shell on navigation. Becoming visible resumes the
    /// busy-poll with an immediate refresh.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if visible && !self.visible {
            self.visible = true;
            self.refresh(cx);
        } else {
            self.visible = visible;
        }
    }

    pub fn set_client(&mut self, client: Client, cx: &mut Context<Self>) {
        self.client = Some(client);
        self.refresh(cx);
    }

    fn busy_status(&self) -> bool {
        is_project_busy(&self.project_status)
    }

    fn read_only(&self) -> bool {
        self.busy_status() || !self.editable
    }

    fn form_disabled(&self) -> bool {
        self.read_only() || self.yaml_dirty || !self.loaded || self.op.is_some()
    }

    fn subtitles(&self) -> bool {
        self.project_fmt == "srt"
    }

    fn is_pdf(&self) -> bool {
        self.project_fmt == "pdf"
    }

    fn precision(&self) -> bool {
        get_str(&self.effective, "pipeline", "translation_mode", "standard") == "best_of_three"
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let (Some(client), pid) = (self.client.clone(), self.pid.clone()) else {
            return;
        };
        self.loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let config = block_on(client.get_config(&pid))?;
                    let project = block_on(client.get_project(&pid))?;
                    let routes = block_on(client.model_routes(&pid)).ok();
                    // Raw capabilities carry `operations[]` (id + default
                    // tier); the typed `capabilities()` drops that field.
                    let caps_raw =
                        block_on(client.get::<serde_json::Value>("/capabilities")).ok();
                    Ok::<_, crate::client::Error>((config, project, routes, caps_raw))
                })
                .await;
            _ = this.update(cx, |view: &mut ProjSettingsView, cx| {
                view.loading = false;
                match result {
                    Ok((config, project, routes, caps_raw)) => {
                        view.apply_config(config);
                        view.project_status = project.project.status;
                        view.project_fmt = project.project.fmt.unwrap_or_default();
                        view.saved_routes = routes;
                        view.routes_error = None;
                        view.error = None;
                        view.loaded = true;
                        view.operations = caps_raw
                            .as_ref()
                            .and_then(|v| v.get("operations"))
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|op| {
                                        Some(OperationCaps {
                                            id: op.get("id")?.as_str()?.to_string(),
                                            tier: op
                                                .get("tier")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("strong")
                                                .to_string(),
                                        })
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        // Drop selects for operations that vanished.
                        let ids: std::collections::HashSet<String> =
                            view.operations.iter().map(|o| o.id.clone()).collect();
                        view.op_selects.retain(|id, _| ids.contains(id));
                    }
                    Err(err) => {
                        view.error = Some(err.to_string());
                    }
                }
                cx.notify();
            });
            // Poll project status while busy (WS lands later); 3s cadence.
            // Only re-arms while visible; navigation resumes via `set_visible`.
            _ = this.update(cx, |view: &mut ProjSettingsView, cx| {
                if view.visible && view.busy_status() {
                    let (Some(client), pid) = (view.client.clone(), view.pid.clone()) else {
                        return;
                    };
                    cx.spawn(async move |this, cx| {
                        cx.background_executor()
                            .timer(std::time::Duration::from_secs(3))
                            .await;
                        _ = this.update(cx, |view: &mut ProjSettingsView, cx| {
                            if view.visible {
                                view.refresh(cx);
                            }
                        });
                    })
                    .detach();
                }
            });
        })
        .detach();
    }

    fn apply_config(&mut self, config: ProjectConfigOut) {
        self.pending_yaml = Some(config.yaml);
        self.effective = config.effective;
        self.registered_models = config.registered_models;
        self.editable = config.editable;
        self.yaml_dirty = false;
        self.typing_int = None;
        self.sync_form_widgets();
    }

    /// Push programmatic effective values into the form widgets.
    fn sync_form_widgets(&mut self) {
        self.pending_ints = [
            Some(get_i64(&self.effective, "segment", "max_tokens_per_batch", 1800).to_string()),
            Some(get_i64(&self.effective, "segment", "max_tokens_per_segment", 1200).to_string()),
            Some(get_i64(&self.effective, "pipeline", "review_concurrency", 4).to_string()),
        ];
        self.pdf_backend = get_str(&self.effective, "pipeline", "pdf_backend", "mineru");
    }

    /// Mirror of React `setField`: update effective, re-derive the YAML draft.
    fn set_field(&mut self, group: &str, key: &str, value: serde_json::Value, cx: &mut Context<Self>) {
        self.effective = workflow_field(&self.effective, group, key, value);
        let draft = serde_json::to_string_pretty(&self.effective)
            .unwrap_or_else(|_| "{}".to_string());
        self.pending_yaml = Some(draft);
        // Keep the int widgets in sync except the one being typed.
        let desired = [
            get_i64(&self.effective, "segment", "max_tokens_per_batch", 1800).to_string(),
            get_i64(&self.effective, "segment", "max_tokens_per_segment", 1200).to_string(),
            get_i64(&self.effective, "pipeline", "review_concurrency", 4).to_string(),
        ];
        for (i, d) in desired.into_iter().enumerate() {
            if Some(i) != self.typing_int {
                self.pending_ints[i] = Some(d);
            }
        }
        self.typing_int = None;
        self.pdf_backend = get_str(&self.effective, "pipeline", "pdf_backend", "mineru");
        cx.notify();
    }

    /// Set a per-operation model/tier override (mirrors the React
    /// `ModelSelection` onChange): update effective + re-derive the YAML
    /// draft. Clears to the default tier exactly like the source.
    fn set_operation_route(&mut self, op: &str, value: &str, default_tier: &str, cx: &mut Context<Self>) {
        if self.form_disabled() {
            return;
        }
        self.effective = operation_route(&self.effective, op, value, default_tier);
        let draft = serde_json::to_string_pretty(&self.effective)
            .unwrap_or_else(|_| "{}".to_string());
        self.pending_yaml = Some(draft);
        cx.notify();
    }
    fn on_int_changed(&mut self, idx: usize, cx: &mut Context<Self>) {
        if self.form_disabled() {
            return;
        }
        let entity = match idx {
            0 => &self.batch_tokens,
            1 => &self.segment_tokens,
            _ => &self.review_concurrency,
        };
        let raw = entity.read(cx).value().to_string();
        let Ok(v) = raw.trim().parse::<i64>() else {
            return;
        };
        if v < 1 {
            return;
        }
        self.typing_int = Some(idx);
        let (group, key) = match idx {
            0 => ("segment", "max_tokens_per_batch"),
            1 => ("segment", "max_tokens_per_segment"),
            _ => ("pipeline", "review_concurrency"),
        };
        self.set_field(group, key, serde_json::Value::from(v), cx);
    }

    fn toggle_pipeline(&mut self, key: &'static str, checked: bool, cx: &mut Context<Self>) {
        if self.form_disabled() {
            return;
        }
        self.set_field("pipeline", key, serde_json::Value::from(checked), cx);
    }

    fn do_save(&mut self, cx: &mut Context<Self>) {
        let (Some(client), pid) = (self.client.clone(), self.pid.clone()) else {
            return;
        };
        let yaml_text = self.yaml.read(cx).value().to_string();
        self.op = Some("save");
        self.error = None;
        self.notice = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { block_on(client.save_config(&pid, &yaml_text)) })
                .await;
            _ = this.update(cx, |view: &mut ProjSettingsView, cx| {
                view.op = None;
                match result {
                    Ok(config) => {
                        view.apply_config(config);
                        view.notice = Some("Project settings saved".into());
                    }
                    Err(err) => {
                        view.error = Some(err.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn do_validate(&mut self, cx: &mut Context<Self>) {
        let (Some(client), pid) = (self.client.clone(), self.pid.clone()) else {
            return;
        };
        let yaml_text = self.yaml.read(cx).value().to_string();
        self.op = Some("validate");
        self.error = None;
        self.notice = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { block_on(client.validate_config(&pid, &yaml_text)) })
                .await;
            _ = this.update(cx, |view: &mut ProjSettingsView, cx| {
                view.op = None;
                match result {
                    Ok(value) => {
                        if let Some(y) = value.get("yaml").and_then(|v| v.as_str()) {
                            view.pending_yaml = Some(y.to_string());
                        }
                        if let Some(e) = value.get("effective") {
                            view.effective = e.clone();
                            view.sync_form_widgets();
                        }
                        view.validated_routes = value.get("routes").cloned();
                        view.yaml_dirty = false;
                        view.typing_int = None;
                        view.notice =
                            Some("Configuration is valid but not saved yet".into());
                    }
                    Err(err) => {
                        view.error = Some(err.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn do_restore(&mut self, cx: &mut Context<Self>) {
        let (Some(client), pid) = (self.client.clone(), self.pid.clone()) else {
            return;
        };
        self.op = Some("restore");
        self.error = None;
        self.notice = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { block_on(client.get_project_defaults(&pid)) })
                .await;
            _ = this.update(cx, |view: &mut ProjSettingsView, cx| {
                view.op = None;
                match result {
                    Ok(config) => {
                        view.apply_config(config);
                        view.notice =
                            Some("Defaults loaded into the draft. Save to apply.".into());
                    }
                    Err(err) => {
                        view.error = Some(err.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn do_check(&mut self, cx: &mut Context<Self>) {
        let (Some(client), pid) = (self.client.clone(), self.pid.clone()) else {
            return;
        };
        let workflow = if self.subtitles() { "srt" } else { "translate" }.to_string();
        self.checking = true;
        self.check_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { block_on(client.check_models(&pid, &workflow)) })
                .await;
            _ = this.update(cx, |view: &mut ProjSettingsView, cx| {
                view.checking = false;
                match result {
                    Ok(value) => {
                        view.check_result = Some(value);
                    }
                    Err(err) => {
                        view.check_error = Some(err.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn model_detail(&self, id: &str) -> String {
        self.registered_models
            .get(id)
            .map(|m| {
                let provider = m.get("provider").and_then(|v| v.as_str()).unwrap_or("?");
                let model = m.get("model").and_then(|v| v.as_str()).unwrap_or(id);
                format!("{provider} / {model}")
            })
            .unwrap_or_default()
    }
}

impl Render for ProjSettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current_palette();

        // Apply deferred widget syncs (set_value needs Window).
        if let Some(text) = self.pending_yaml.take() {
            if self.yaml.read(cx).value().to_string() != text {
                self.yaml.update(cx, |state, cx| {
                    state.set_value(text, window, cx);
                });
            }
        }
        {
            let entities = [
                self.batch_tokens.clone(),
                self.segment_tokens.clone(),
                self.review_concurrency.clone(),
            ];
            for (i, entity) in entities.into_iter().enumerate() {
                if let Some(text) = self.pending_ints[i].take() {
                    if entity.read(cx).value().to_string() != text {
                        entity.update(cx, |state, cx| {
                            state.set_value(text, window, cx);
                        });
                    }
                }
            }
        }
        // Lazily build the pdf-backend select (options are fixed).
        if self.pdf_select.is_none() {
            let opts = vec!["mineru".to_string(), "babeldoc".to_string()];
            let idx = opts.iter().position(|o| o == &self.pdf_backend).unwrap_or(0);
            let entity = cx.new(|cx| {
                SelectState::new(opts, Some(IndexPath::new(idx)), window, cx)
            });
            cx.subscribe(
                &entity,
                |this: &mut ProjSettingsView, _, event: &SelectEvent<Vec<String>>, cx| {
                    if let SelectEvent::Confirm(Some(value)) = event {
                        let value = value.clone();
                        this.pdf_backend = value.clone();
                        if !this.form_disabled() {
                            this.set_field(
                                "pipeline",
                                "pdf_backend",
                                serde_json::Value::from(value),
                                cx,
                            );
                        }
                    }
                },
            )
            .detach();
            self.pdf_select = Some(entity);
        }
        // Keep the select in sync with programmatic effective changes.
        if let Some(entity) = self.pdf_select.clone() {
            if self.pdf_backend
                != get_str(&self.effective, "pipeline", "pdf_backend", "mineru")
            {
                self.pdf_backend =
                    get_str(&self.effective, "pipeline", "pdf_backend", "mineru");
            }
            // Reflect the tracked value without emitting Confirm.
            entity.update(cx, |state, cx| {
                state.set_selected_value(&self.pdf_backend, window, cx);
            });
        }

        let read_only = self.read_only();
        let form_disabled = self.form_disabled();
        let op_busy = self.op.is_some();

        // Scroll region owns overflow; flex_1 + min_h_0 lets it shrink in
        // the app-shell column.
        let mut col = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_3()
            .p_6()
            .overflow_y_scrollbar()
            .child(div().text_xl().font_weight(FontWeight::BOLD).child("Project settings"));

        if let Some(err) = &self.error {
            col = col.child(
                div()
                    .p_3()
                    .rounded_md()
                    .bg(palette.destructive)
                    .text_color(palette.destructive_foreground)
                    .child(format!("Error: {err}")),
            );
        }
        if let Some(notice) = &self.notice {
            col = col.child(
                div()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(palette.border)
                    .child(notice.clone()),
            );
        }
        if self.loading && !self.loaded {
            col = col.child(
                div().text_sm().text_color(palette.muted_foreground).child("Loading…"),
            );
            return col;
        }
        if read_only {
            col = col.child(
                div()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(palette.border)
                    .text_sm()
                    .text_color(palette.muted_foreground)
                    .child(if self.busy_status() {
                        "Project is running — settings are read-only."
                    } else {
                        "Configuration is not editable for this project."
                    }),
            );
        }

        // -- Models section: tier map (read-only note) + per-operation
        //    overrides (Select per caps.operations, mirrors ModelSelection).
        {
            let mut body = div().flex().flex_col().gap_2();
            for (id, label) in TIERS {
                let tiers = section(&self.effective, "llm");
                let model_id = tiers
                    .get("tiers")
                    .and_then(|t| t.get(*id))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let detail = if model_id.is_empty() {
                    String::new()
                } else {
                    self.model_detail(model_id)
                };
                body = body.child(
                    div()
                        .flex()
                        .gap_2()
                        .text_sm()
                        .child(div().w_32().text_color(palette.muted_foreground).child(*label))
                        .child(div().child(if model_id.is_empty() {
                            "—".to_string()
                        } else if detail.is_empty() {
                            model_id.to_string()
                        } else {
                            format!("{model_id} · {detail}")
                        })),
                );
            }
            if self.operations.is_empty() {
                body = body.child(
                    div()
                        .text_sm()
                        .text_color(palette.muted_foreground)
                        .child("Per-operation overrides unavailable (capabilities carry no operations)."),
                );
            } else {
                body = body.child(div().text_sm().font_medium().child("Operation models"));
                let ops = self.operations.clone();
                // Registered model ids for the option lists.
                let model_ids: Vec<String> = self
                    .registered_models
                    .as_object()
                    .map(|m| m.keys().cloned().collect())
                    .unwrap_or_default();
                for op in &ops {
                    let op_id = op.id.clone();
                    let default_tier = op.tier.clone();
                    // Current value: `model` or `tier:<id>` (mirrors React).
                    let routes = section(&self.effective, "llm");
                    let route = routes.get("routes").and_then(|r| r.get(&op_id));
                    let current = match route.and_then(|r| r.get("model")).and_then(|v| v.as_str()) {
                        Some(m) => m.to_string(),
                        None => format!(
                            "tier:{}",
                            route
                                .and_then(|r| r.get("tier"))
                                .and_then(|v| v.as_str())
                                .unwrap_or(&default_tier)
                        ),
                    };
                    // Options: quality/economy/fast tiers + registered models.
                    let mut opts = vec![
                        "tier:strong".to_string(),
                        "tier:cheap".to_string(),
                        "tier:fast".to_string(),
                    ];
                    for m in &model_ids {
                        if !opts.iter().any(|o| o == m) {
                            opts.push(m.clone());
                        }
                    }
                    let entity = match self.op_selects.get(&op_id) {
                        Some(e) => e.clone(),
                        None => {
                            let idx = opts.iter().position(|o| o == &current).unwrap_or(0);
                            let opts_clone = opts.clone();
                            let op_clone = op_id.clone();
                            let tier_clone = default_tier.clone();
                            let entity = cx.new(|cx| {
                                SelectState::new(opts_clone, Some(IndexPath::new(idx)), window, cx)
                            });
                            cx.subscribe(
                                &entity,
                                move |this: &mut ProjSettingsView, _, event: &SelectEvent<Vec<String>>, cx| {
                                    if let SelectEvent::Confirm(Some(value)) = event {
                                        this.set_operation_route(&op_clone, value, &tier_clone, cx);
                                    }
                                },
                            )
                            .detach();
                            self.op_selects.insert(op_id.clone(), entity.clone());
                            entity
                        }
                    };
                    // Reflect programmatic changes without emitting Confirm.
                    {
                        let current = current.clone();
                        entity.update(cx, |state, cx| {
                            state.set_selected_value(&current, window, cx);
                        });
                    }
                    body = body.child(
                        div().flex().flex_col().gap_1()
                            .child(div().text_sm().child(op_id.clone()))
                            .child(Select::new(&entity)),
                    );
                }
            }
            body = body.child(
                div()
                    .text_sm()
                    .text_color(palette.muted_foreground)
                    .child("Manage models in global Settings"),
            );
            col = col.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_4()
                    .rounded_md()
                    .border_1()
                    .border_color(palette.border)
                    .child(div().font_medium().child("Models"))
                    .child(body),
            );
        }

        // -- Workflow section --
        {
            let mut body = div().flex().flex_col().gap_2();
            if !self.subtitles() {
                let pipeline = section(&self.effective, "pipeline");
                let toggles: [(&str, &str); 4] = [
                    ("book_understanding", "Book understanding"),
                    ("polish", "Polishing"),
                    ("review", "Whole-book review"),
                    (
                        "review_autofix",
                        "Apply autofixes to the saved translation after review",
                    ),
                ];
                for (key, label) in toggles {
                    let checked = pipeline.get(key).and_then(|v| v.as_bool()).unwrap_or(false);
                    let disabled_key = key == "polish" && self.precision();
                    let key_static: &'static str = match key {
                        "book_understanding" => "book_understanding",
                        "polish" => "polish",
                        "review" => "review",
                        _ => "review_autofix",
                    };
                    body = body.child(
                        Checkbox::new(format!("wf-{key}"))
                            .label(label)
                            .checked(checked)
                            .disabled(form_disabled || disabled_key)
                            .on_change(cx.listener(move |this, checked: &bool, _, cx| {
                                this.toggle_pipeline(key_static, *checked, cx);
                            })),
                    );
                }
            }
            // Performance group.
            let mut perf = div().flex().flex_col().gap_2().child(
                div().text_sm().font_medium().child("Segmentation and performance"),
            );
            if !self.subtitles() {
                let checked = get_bool(&self.effective, "pipeline", "annotation_alignment");
                perf = perf.child(
                    Checkbox::new("wf-annotation-alignment")
                        .label("Paragraph annotation alignment")
                        .checked(checked)
                        .disabled(form_disabled)
                        .on_change(cx.listener(|this, checked: &bool, _, cx| {
                            this.toggle_pipeline("annotation_alignment", *checked, cx);
                        })),
                );
            }
            perf = perf
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_sm().child("Tokens per batch"))
                        .child(Input::new(&self.batch_tokens).disabled(form_disabled)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_sm().child("Tokens per paragraph"))
                        .child(Input::new(&self.segment_tokens).disabled(form_disabled)),
                );
            if !self.subtitles() {
                perf = perf.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_sm().child("Review concurrency"))
                        .child(Input::new(&self.review_concurrency).disabled(form_disabled)),
                );
            }
            if self.is_pdf() {
                perf = perf.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_sm().child("PDF parser"))
                        .child(Select::new(self.pdf_select.as_ref().unwrap())),
                );
            }
            body = body.child(perf);
            col = col.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_4()
                    .rounded_md()
                    .border_1()
                    .border_color(palette.border)
                    .child(div().font_medium().child("Workflow"))
                    .child(body),
            );
        }

        // -- Advanced section --
        {
            let mut body = div().flex().flex_col().gap_2();
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_sm().font_medium().child("Advanced YAML configuration"))
                    .child(if self.yaml_dirty {
                        div()
                            .text_sm()
                            .text_color(palette.muted_foreground)
                            .child("Unsaved changes")
                    } else {
                        div()
                    })
                    .child(
                        Textarea::new(&self.yaml)
                            .h(px(420.0))
                            .disabled(read_only || !self.loaded),
                    ),
            );
            if let Some(routes) = &self.validated_routes {
                let pretty = serde_json::to_string_pretty(routes).unwrap_or_default();
                body = body.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .p_3()
                        .rounded_md()
                        .border_1()
                        .border_color(palette.border)
                        .child(div().text_sm().font_medium().child("Validated model routes"))
                        .child(div().text_xs().child(pretty)),
                );
            }
            // Saved model routes + check.
            {
                let mut routes_block = div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_sm().font_medium().child("Saved model routes"))
                    .child(
                        Button::new("check-models")
                            .label(if self.checking {
                                "Checking…"
                            } else {
                                "Check model configuration"
                            })
                            .disabled(self.checking)
                            .on_click(cx.listener(|this, _, _, cx| this.do_check(cx))),
                    );
                if let Some(err) = self.routes_error.clone().or(self.check_error.clone()) {
                    routes_block = routes_block.child(
                        div().text_sm().text_color(palette.destructive).child(err),
                    );
                }
                if let Some(routes) = &self.saved_routes {
                    let mut table = div().flex().flex_col().gap_1().child(
                        div()
                            .flex()
                            .gap_3()
                            .text_xs()
                            .text_color(palette.muted_foreground)
                            .child(div().w_32().child("Operation"))
                            .child(div().flex_1().child("Model profile / model"))
                            .child(div().w_32().child("Provider"))
                            .child(div().w_24().child("Tier"))
                            .child(div().flex_1().child("Fallback models")),
                    );
                    for route in routes {
                        let operation = route
                            .get("operation")
                            .and_then(|v| v.as_str())
                            .unwrap_or("—")
                            .to_string();
                        let profile = route
                            .get("profile")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                            .or_else(|| {
                                route.get("model").and_then(|v| v.as_str()).map(|s| s.to_string())
                            })
                            .unwrap_or_else(|| "—".to_string());
                        let model = route
                            .get("model")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let provider = route
                            .get("provider")
                            .and_then(|v| v.as_str())
                            .unwrap_or("—")
                            .to_string();
                        let tier = route
                            .get("tier")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Custom")
                            .to_string();
                        let fallbacks = route
                            .get("fallbacks")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            })
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| "—".to_string());
                        table = table.child(
                            div()
                                .flex()
                                .gap_3()
                                .text_sm()
                                .child(div().w_32().text_xs().child(operation))
                                .child(
                                    div().flex_1().child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .child(profile)
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(palette.muted_foreground)
                                                    .child(model),
                                            ),
                                    ),
                                )
                                .child(div().w_32().child(provider))
                                .child(div().w_24().child(tier))
                                .child(div().flex_1().child(fallbacks)),
                        );
                    }
                    routes_block = routes_block.child(table);
                }
                if let Some(result) = &self.check_result {
                    let pretty = serde_json::to_string_pretty(result).unwrap_or_default();
                    routes_block = routes_block.child(
                        div()
                            .p_3()
                            .rounded_md()
                            .border_1()
                            .border_color(palette.border)
                            .child(div().text_xs().child(pretty)),
                    );
                }
                body = body.child(routes_block);
            }
            col = col.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_4()
                    .rounded_md()
                    .border_1()
                    .border_color(palette.border)
                    .child(div().font_medium().child("Advanced"))
                    .child(body),
            );
        }

        // -- Action bar --
        {
            let mut bar = div().flex().items_center().gap_2();
            if self.yaml_dirty {
                bar = bar.child(
                    div()
                        .flex_1()
                        .text_sm()
                        .text_color(palette.muted_foreground)
                        .child("Advanced YAML has unvalidated changes. Validate it to update the form."),
                );
            } else {
                bar = bar.child(div().flex_1());
            }
            let op = self.op;
            bar = bar
                .child(
                    Button::new("restore-defaults")
                        .label("Restore defaults")
                        .disabled(!self.loaded || read_only || op_busy)
                        .on_click(cx.listener(|this, _, _, cx| this.do_restore(cx))),
                )
                .child(
                    Button::new("validate")
                        .label(if op == Some("validate") {
                            "Validating…"
                        } else {
                            "Validate configuration"
                        })
                        .disabled(!self.loaded || read_only || op_busy)
                        .on_click(cx.listener(|this, _, _, cx| this.do_validate(cx))),
                )
                .child(
                    Button::new("save")
                        .label(if op == Some("save") { "Saving…" } else { "Save configuration" })
                        .disabled(!self.loaded || read_only || op_busy)
                        .on_click(cx.listener(|this, _, _, cx| this.do_save(cx))),
                );
            col = col.child(bar);
        }

        col
    }
}

#[cfg(test)]
mod tests {
    use super::operation_route;

    #[test]
    fn operation_route_sets_and_clears_default_tier() {
        let base = serde_json::json!({"llm": {"tiers": {"strong": "m"}}});
        let set = operation_route(&base, "translate", "m2", "strong");
        assert_eq!(
            set.pointer("/llm/routes/translate/model").and_then(|v| v.as_str()),
            Some("m2")
        );
        let tiered = operation_route(&set, "translate", "tier:strong", "strong");
        assert_eq!(tiered.pointer("/llm/routes/translate"), None);
        let cheap = operation_route(&base, "translate", "tier:cheap", "strong");
        assert_eq!(
            cheap.pointer("/llm/routes/translate/tier").and_then(|v| v.as_str()),
            Some("cheap")
        );
    }
}
