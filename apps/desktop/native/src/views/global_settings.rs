//! Global settings (screen 13): interface language, providers & models,
//! default tiers + per-operation routes, workflow, MinerU, advanced YAML.
//!
//! Parity source: `features/settings/InterfaceSettingsPage.tsx`,
//! `GlobalConfiguration.tsx`, `LanguageSettings.tsx`, `ProviderSettings.tsx`,
//! `ModelSelection.tsx`, `ModelConnectivity.tsx`, `ModelNameField.tsx`,
//! `RegistryIdField.tsx`, `WorkflowSettings.tsx` (global scope),
//! `MinerUSettings.tsx`.
//! - Language Select (en / zh-CN) is a local view pref, no backend.
//! - Provider registry: connection id/kind/base_url/api_key_env/timeout +
//!   add/delete; kind change resets base_url/api_key_env (mirrors source).
//! - Per-connection credential PasswordInput: statuses via
//!   `credential_statuses`; typed keys are pending-write tracked and committed
//!   via `save_credential` after the configuration save succeeds (mirrors the
//!   `prepareCommit` flow). No reveal endpoint exists in `Client`, so saved
//!   keys are never shown — only availability from statuses.
//! - Model registry: model id/connection/model-name + `list_models_remote`
//!   suggestions. Tier selects (strong/cheap/fast) + per-operation routes
//!   with fallbacks display.
//! - Global workflow toggles + segment/perf (no translation_mode) + pdf
//!   backend (mineru/babeldoc). MinerU password with autosave (debounce
//!   800ms + blur via `save_mineru`).
//! - Advanced YAML editor (multiline, no LSP); yamlDirty gates the form.
//! - Ping via `ping_models` (saved-only gate). Restore/Validate/Save with
//!   revision optimistic concurrency (409 → surface error + reload).
//! - Updater: the Tauri updater section is not ported; a placeholder notes it.

use std::collections::{HashMap, HashSet};

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Disableable, IndexPath, StyledExt};
use gpui_kit::*;

use crate::{
    client::{block_on, Client, GlobalConfigOut},
    theme,
};

/// Tier ids in display order — mirrors `tierNames` in `ModelSelection.tsx`.
const TIERS: &[(&str, &str)] = &[
    ("strong", "Quality tier"),
    ("cheap", "Economy tier"),
    ("fast", "Fast tier"),
];

/// Language options: (code, label). Local pref only.
const LOCALES: &[(&str, &str)] = &[("en", "English"), ("zh-CN", "简体中文")];

fn section(value: &serde_json::Value, key: &str) -> serde_json::Value {
    value.get(key).cloned().unwrap_or(serde_json::Value::Null)
}

fn llm(value: &serde_json::Value) -> serde_json::Value {
    value
        .get("llm")
        .cloned()
        .filter(|v| v.is_object())
        .unwrap_or_else(|| serde_json::Value::Object(Default::default()))
}

fn get_str(value: &serde_json::Value, group: &str, key: &str, default: &str) -> String {
    value
        .get(group)
        .and_then(|g| g.get(key))
        .and_then(|v| v.as_str())
        .unwrap_or(default)
        .to_string()
}

fn get_i64(value: &serde_json::Value, group: &str, key: &str, default: i64) -> i64 {
    value
        .get(group)
        .and_then(|g| g.get(key))
        .and_then(|v| v.as_i64())
        .unwrap_or(default)
}

fn get_bool(value: &serde_json::Value, group: &str, key: &str) -> bool {
    value
        .get(group)
        .and_then(|g| g.get(key))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
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

/// Set `llm[group][id][key] = value` on the effective object.
fn set_registry_field(
    effective: &serde_json::Value,
    group: &str,
    id: &str,
    key: &str,
    value: serde_json::Value,
) -> serde_json::Value {
    let mut l = llm(effective);
    let mut entries = l
        .get(group)
        .cloned()
        .filter(|v| v.is_object())
        .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
    let mut entry = entries
        .get(id)
        .cloned()
        .filter(|v| v.is_object())
        .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
    if let Some(map) = entry.as_object_mut() {
        map.insert(key.to_string(), value);
    }
    if let Some(map) = entries.as_object_mut() {
        map.insert(id.to_string(), entry);
    }
    let mut next = effective.clone();
    if !next.is_object() {
        next = serde_json::Value::Object(Default::default());
    }
    if let Some(map) = next.as_object_mut() {
        map.insert("llm".to_string(), l);
    }
    next
}

/// Mirror of `renameRegistryId` in `registryEdits.ts`.
fn rename_registry_id(
    effective: &serde_json::Value,
    group: &str,
    old_id: &str,
    new_id: &str,
) -> serde_json::Value {
    let l = llm(effective);
    let remap = |entries: &serde_json::Value| {
        let mut out = serde_json::Map::new();
        if let Some(map) = entries.as_object() {
            for (id, value) in map {
                out.insert(
                    if id == old_id {
                        new_id.to_string()
                    } else {
                        id.clone()
                    },
                    value.clone(),
                );
            }
        }
        serde_json::Value::Object(out)
    };
    let mut next_llm = l.clone();
    if let Some(map) = next_llm.as_object_mut() {
        map.insert(
            group.to_string(),
            remap(&l.get(group).cloned().unwrap_or(serde_json::Value::Null)),
        );
        map.insert("preset".to_string(), serde_json::Value::Null);
        if group == "providers" {
            let mut models = serde_json::Map::new();
            if let Some(map) = l.get("models").and_then(|v| v.as_object()) {
                for (id, raw) in map {
                    let mut model = raw.clone();
                    if model.get("provider").and_then(|v| v.as_str()) == Some(old_id) {
                        if let Some(m) = model.as_object_mut() {
                            m.insert(
                                "provider".to_string(),
                                serde_json::Value::from(new_id),
                            );
                        }
                    }
                    models.insert(id.clone(), model);
                }
            }
            map.insert("models".to_string(), serde_json::Value::Object(models));
        } else {
            let mut tiers = serde_json::Map::new();
            if let Some(map) = l.get("tiers").and_then(|v| v.as_object()) {
                for (tier, id) in map {
                    tiers.insert(
                        tier.clone(),
                        if id.as_str() == Some(old_id) {
                            serde_json::Value::from(new_id)
                        } else {
                            id.clone()
                        },
                    );
                }
            }
            map.insert("tiers".to_string(), serde_json::Value::Object(tiers));
            let mut routes = serde_json::Map::new();
            if let Some(map) = l.get("routes").and_then(|v| v.as_object()) {
                for (id, raw) in map {
                    let mut route = raw.clone();
                    if let Some(m) = route.as_object_mut() {
                        if m.get("model").and_then(|v| v.as_str()) == Some(old_id) {
                            m.insert("model".to_string(), serde_json::Value::from(new_id));
                        }
                        if let Some(fb) = m.get("fallbacks").and_then(|v| v.as_array()) {
                            let next: Vec<serde_json::Value> = fb
                                .iter()
                                .map(|f| {
                                    if f.as_str() == Some(old_id) {
                                        serde_json::Value::from(new_id)
                                    } else {
                                        f.clone()
                                    }
                                })
                                .collect();
                            m.insert("fallbacks".to_string(), serde_json::Value::from(next));
                        }
                    }
                    routes.insert(id.clone(), route);
                }
            }
            map.insert("routes".to_string(), serde_json::Value::Object(routes));
        }
    }
    let mut next = effective.clone();
    if let Some(map) = next.as_object_mut() {
        map.insert("llm".to_string(), next_llm);
    }
    next
}

/// Mirror of `registryReferences` in `registryEdits.ts`.
fn registry_references(effective: &serde_json::Value, group: &str, id: &str) -> Vec<String> {
    let l = llm(effective);
    if group == "providers" {
        return l
            .get("models")
            .and_then(|v| v.as_object())
            .map(|map| {
                map.iter()
                    .filter(|(_, m)| m.get("provider").and_then(|v| v.as_str()) == Some(id))
                    .map(|(k, _)| k.clone())
                    .collect()
            })
            .unwrap_or_default();
    }
    let mut refs = Vec::new();
    if let Some(map) = l.get("tiers").and_then(|v| v.as_object()) {
        for (tier, model) in map {
            if model.as_str() == Some(id) {
                refs.push(tier.clone());
            }
        }
    }
    if let Some(map) = l.get("routes").and_then(|v| v.as_object()) {
        for (op, raw) in map {
            let used = raw.get("model").and_then(|v| v.as_str()) == Some(id)
                || raw
                    .get("fallbacks")
                    .and_then(|v| v.as_array())
                    .map(|fb| fb.iter().any(|f| f.as_str() == Some(id)))
                    .unwrap_or(false);
            if used {
                refs.push(op.clone());
            }
        }
    }
    refs
}

fn category_message(category: &str) -> &'static str {
    match category {
        "ok" => "The model replied.",
        "reachable" => "The model accepted the request; the reply was empty or truncated.",
        "missing_credential" => "No API key is configured for this connection.",
        "authentication_failed" => "Authentication failed. Check the API key.",
        "insufficient_balance" => "Insufficient account balance.",
        "permission_denied" => "Access denied. Check account and model permissions.",
        "model_or_endpoint_not_found" => "Model or endpoint not found. Check the model name and base URL.",
        "invalid_request" => "The provider rejected the request. Check the model settings.",
        "rate_limited" => "Rate limit reached. Try again later.",
        "timeout" => "The request timed out.",
        "connection_failed" => "Could not connect. Check the base URL and network.",
        "provider_unavailable" => "The provider is unavailable. Try again later.",
        _ => "Probe failed.",
    }
}

pub struct GlobalSettingsView {
    client: Option<Client>,
    loading: bool,
    loaded: bool,
    error: Option<String>,
    notice: Option<String>,
    effective: serde_json::Value,
    saved_yaml: Option<String>,
    revision: i64,
    provider_kinds: Vec<String>,
    operations: Vec<serde_json::Value>,
    cred_statuses: serde_json::Value,
    language: String,
    lang_select: Option<Entity<SelectState<Vec<String>>>>,
    theme_choice: theme::ThemeChoice,
    theme_mode_select: Option<Entity<SelectState<Vec<String>>>>,
    theme_accent_select: Option<Entity<SelectState<Vec<String>>>>,
    app_handle: Option<WeakEntity<crate::views::App>>,
    yaml: Entity<TextareaState>,
    pending_yaml: Option<String>,
    yaml_dirty: bool,
    uncommitted: bool,
    op: Option<&'static str>,
    inputs: HashMap<String, Entity<InputState>>,
    pending_text: HashMap<String, String>,
    cred_drafts: HashMap<String, String>,
    /// MinerU key field state.
    mineru_input: Entity<InputState>,
    mineru_pending: Option<String>,
    mineru_status: serde_json::Value,
    mineru_dirty: bool,
    mineru_saving: bool,
    mineru_gen: u64,
    rename_inputs: HashMap<String, Entity<InputState>>,
    rename_pending: HashMap<String, String>,
    /// "group:id" keys with an uncommitted ID edit.
    editing_ids: HashSet<String>,
    selects: HashMap<String, Entity<SelectState<Vec<String>>>>,
    select_sigs: HashMap<String, String>,
    tier_values: HashMap<String, String>,
    op_values: HashMap<String, String>,
    pdf_backend: String,
    batch_tokens: Entity<InputState>,
    segment_tokens: Entity<InputState>,
    review_concurrency: Entity<InputState>,
    pending_ints: [Option<String>; 3],
    typing_int: Option<usize>,
    suggestions: HashMap<String, Vec<String>>,
    suggestion_error: HashMap<String, String>,
    fetching_models: HashSet<String>,
    ping_results: Option<Vec<serde_json::Value>>,
    ping_error: Option<String>,
    pinging: bool,
}

impl GlobalSettingsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let yaml = cx.new(|cx| TextareaState::new(window, cx));
        cx.subscribe(&yaml, |this: &mut GlobalSettingsView, _, _: &InputEvent, cx| {
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
                move |this: &mut GlobalSettingsView, _, _: &InputEvent, cx| {
                    this.on_int_changed(idx, cx);
                },
            )
            .detach();
        }
        let mineru_input =
            cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("MinerU API key"));
        cx.subscribe(
            &mineru_input,
            |this: &mut GlobalSettingsView, _, event: &InputEvent, cx| {
                match event {
                    InputEvent::Blur => this.save_mineru_now(cx),
                    InputEvent::Change => this.on_mineru_changed(cx),
                    _ => {}
                }
            },
        )
        .detach();
        Self {
            client: None,
            loading: true,
            loaded: false,
            error: None,
            notice: None,
            effective: serde_json::Value::Object(Default::default()),
            saved_yaml: None,
            revision: 0,
            provider_kinds: Vec::new(),
            operations: Vec::new(),
            cred_statuses: serde_json::Value::Object(Default::default()),
            language: "en".into(),
            theme_choice: theme::load_choice(),
            lang_select: None,
            yaml,
            theme_mode_select: None,
            theme_accent_select: None,
            app_handle: None,
            pending_yaml: None,
            uncommitted: false,
            yaml_dirty: false,
            op: None,
            inputs: HashMap::new(),
            pending_text: HashMap::new(),
            cred_drafts: HashMap::new(),
            mineru_input,
            mineru_pending: None,
            mineru_status: serde_json::Value::Object(Default::default()),
            mineru_dirty: false,
            mineru_saving: false,
            mineru_gen: 0,
            rename_inputs: HashMap::new(),
            rename_pending: HashMap::new(),
            editing_ids: HashSet::new(),
            selects: HashMap::new(),
            select_sigs: HashMap::new(),
            tier_values: HashMap::new(),
            op_values: HashMap::new(),
            pdf_backend: "mineru".into(),
            batch_tokens,
            segment_tokens,
            review_concurrency,
            pending_ints: [None, None, None],
            typing_int: None,
            suggestions: HashMap::new(),
            suggestion_error: HashMap::new(),
            fetching_models: HashSet::new(),
            ping_results: None,
            ping_error: None,
            pinging: false,
        }
    }

    pub fn set_client(&mut self, client: Client, cx: &mut Context<Self>) {
        self.client = Some(client);
        self.refresh(cx);
    }
    pub fn set_app_handle(&mut self, handle: WeakEntity<crate::views::App>) {
        self.app_handle = Some(handle);
    }
    fn providers(&self) -> Vec<(String, serde_json::Value)> {
        llm(&self.effective)
            .get("providers")
            .and_then(|v| v.as_object())
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }

    fn models(&self) -> Vec<(String, serde_json::Value)> {
        llm(&self.effective)
            .get("models")
            .and_then(|v| v.as_object())
            .map(|m| {
                m.iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }

    fn form_disabled(&self) -> bool {
        self.yaml_dirty || !self.loaded || self.op.is_some()
    }

    fn cred_pending(&self) -> bool {
        self.cred_drafts.values().any(|v| !v.trim().is_empty())
    }

    fn dirty(&self) -> bool {
        let yaml_text = self.pending_yaml.clone().unwrap_or_default();
        let saved = self.saved_yaml.clone().unwrap_or_default();
        let config_saved = !yaml_text.is_empty() && yaml_text == saved;
        !config_saved || self.uncommitted || self.yaml_dirty || self.cred_pending() || !self.editing_ids.is_empty()
    }

    fn config_saved(&self) -> bool {
        match (&self.pending_yaml, &self.saved_yaml) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let config = block_on(client.get_global_config())?;
                    let caps = block_on(client.get::<serde_json::Value>("/capabilities")).ok();
                    let creds = block_on(client.credential_statuses()).ok();
                    let mineru = block_on(client.mineru_status()).ok();
                    Ok::<_, crate::client::Error>((config, caps, creds, mineru))
                })
                .await;
            _ = this.update(cx, |view: &mut GlobalSettingsView, cx| {
                view.loading = false;
                match result {
                    Ok((config, caps, creds, mineru)) => {
                        view.apply_config(config);
                        if let Some(caps) = caps {
                            view.provider_kinds = caps
                                .get("providers")
                                .and_then(|v| v.as_array())
                                .map(|a| {
                                    a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
                                })
                                .unwrap_or_default();
                            view.provider_kinds.retain(|k| k != "fake");
                            view.operations = caps
                                .get("operations")
                                .and_then(|v| v.as_array())
                                .cloned()
                                .unwrap_or_default();
                        }
                        if let Some(creds) = creds {
                            view.cred_statuses = creds;
                        }
                        if let Some(mineru) = mineru {
                            view.mineru_status = mineru;
                        }
                        view.loaded = true;
                        view.error = None;
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

    fn apply_config(&mut self, config: GlobalConfigOut) {
        self.revision = config.revision;
        self.saved_yaml = Some(config.yaml.clone());
        self.pending_yaml = Some(config.yaml);
        self.effective = config.effective;
        self.yaml_dirty = false;
        self.typing_int = None;
        self.editing_ids.clear();
        self.suggestions.clear();
        self.suggestion_error.clear();
        self.ping_results = None;
        self.ping_error = None;
        self.sync_form_widgets();
    }

    fn sync_form_widgets(&mut self) {
        self.pending_ints = [
            Some(get_i64(&self.effective, "segment", "max_tokens_per_batch", 1800).to_string()),
            Some(get_i64(&self.effective, "segment", "max_tokens_per_segment", 1200).to_string()),
            Some(get_i64(&self.effective, "pipeline", "review_concurrency", 4).to_string()),
        ];
        self.pdf_backend = get_str(&self.effective, "pipeline", "pdf_backend", "mineru");
        self.tier_values.clear();
        for (id, _) in TIERS {
            let v = section(&llm(&self.effective), "tiers")
                .get(*id)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            self.tier_values.insert(id.to_string(), v);
        }
        self.op_values.clear();
        for op in &self.operations {
            let oid = op.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if oid.is_empty() {
                continue;
            }
            let route = section(&llm(&self.effective), "routes");
            let r = route.get(&oid).cloned().unwrap_or(serde_json::Value::Null);
            let value = r
                .get("model")
                .and_then(|v| v.as_str())
                .map(|m| m.to_string())
                .unwrap_or_else(|| {
                    format!("tier:{}", r.get("tier").and_then(|v| v.as_str()).unwrap_or(""))
                });
            self.op_values.insert(oid, value);
        }
    }

    /// Mirror of React `setField`: update effective, re-derive the YAML draft.
    fn set_field(&mut self, group: &str, key: &str, value: serde_json::Value, cx: &mut Context<Self>) {
        self.effective = workflow_field(&self.effective, group, key, value);
        self.rederive_yaml(cx);
    }

    fn set_llm(&mut self, next_llm: serde_json::Value, cx: &mut Context<Self>) {
        let mut next = self.effective.clone();
        if !next.is_object() {
            next = serde_json::Value::Object(Default::default());
        }
        if let Some(map) = next.as_object_mut() {
            map.insert("llm".to_string(), next_llm);
        }
        self.effective = next;
        self.rederive_yaml(cx);
    }

    fn rederive_yaml(&mut self, cx: &mut Context<Self>) {
        let draft = serde_json::to_string_pretty(&self.effective).unwrap_or_else(|_| "{}".into());
        self.pending_yaml = Some(draft);
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
        // Tier/op selects track programmatic changes via sig rebuild below.
        self.tier_values.clear();
        for (id, _) in TIERS {
            let v = section(&llm(&self.effective), "tiers")
                .get(*id)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            self.tier_values.insert(id.to_string(), v);
        }
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

    // -- Registry mutations (mirror ProviderSettings) --

    fn update_registry(
        &mut self,
        group: &str,
        id: &str,
        key: &str,
        value: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        if self.form_disabled() {
            return;
        }
        let next = set_registry_field(&self.effective, group, id, key, value);
        self.effective = next;
        self.rederive_yaml(cx);
    }

    fn add_entry(&mut self, group: &str, cx: &mut Context<Self>) {
        if self.form_disabled() {
            return;
        }
        let l = llm(&self.effective);
        let entries = l.get(group).cloned().unwrap_or(serde_json::Value::Null);
        let (prefix, value) = if group == "providers" {
            (
                "provider",
                serde_json::json!({"kind": "openai-compatible"}),
            )
        } else {
            let first = self
                .providers()
                .first()
                .map(|(id, _)| id.clone())
                .unwrap_or_default();
            (
                "model",
                serde_json::json!({"provider": first, "model": "", "options": {}}),
            )
        };
        let mut i = 1;
        let has = |name: &str| entries.get(name).is_some();
        while has(&format!("{prefix}{i}")) {
            i += 1;
        }
        let name = format!("{prefix}{i}");
        let mut next_llm = l.clone();
        if let Some(map) = next_llm.as_object_mut() {
            let mut group_map = map
                .get(group)
                .cloned()
                .filter(|v| v.is_object())
                .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
            if let Some(g) = group_map.as_object_mut() {
                g.insert(name, value);
            }
            map.insert("preset".to_string(), serde_json::Value::Null);
            map.insert(group.to_string(), group_map);
        }
        self.set_llm(next_llm, cx);
    }

    fn remove_entry(&mut self, group: &str, id: &str, cx: &mut Context<Self>) {
        if self.form_disabled() || !registry_references(&self.effective, group, id).is_empty() {
            return;
        }
        let l = llm(&self.effective);
        let mut next_llm = l.clone();
        if let Some(map) = next_llm.as_object_mut() {
            if let Some(g) = map.get(group).and_then(|v| v.as_object()).cloned() {
                let mut g = g;
                g.remove(id);
                map.insert(group.to_string(), serde_json::Value::Object(g));
            }
            map.insert("preset".to_string(), serde_json::Value::Null);
        }
        self.cred_drafts.remove(id);
        self.set_llm(next_llm, cx);
    }

    fn commit_rename(&mut self, group: &str, old_id: &str, new_id: &str, cx: &mut Context<Self>) {
        let new_id = new_id.trim();
        if new_id == old_id || !valid_id(new_id) {
            return;
        }
        let ids: Vec<String> = if group == "providers" {
            self.providers().into_iter().map(|(id, _)| id).collect()
        } else {
            self.models().into_iter().map(|(id, _)| id).collect()
        };
        if ids.iter().any(|id| id == new_id) {
            return;
        }
        self.effective = rename_registry_id(&self.effective, group, old_id, new_id);
        if group == "providers" {
            if let Some(draft) = self.cred_drafts.remove(old_id) {
                self.cred_drafts.insert(new_id.to_string(), draft);
            }
        }
        self.editing_ids.remove(&format!("{group}:{old_id}"));
        self.rederive_yaml(cx);
    }

    // -- Save / validate / restore --

    fn do_save(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let yaml_text = self.yaml.read(cx).value().to_string();
        let revision = self.revision;
        let pending_creds: Vec<(String, String)> = self
            .cred_drafts
            .iter()
            .filter(|(_, v)| !v.trim().is_empty())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        self.op = Some("save");
        self.error = None;
        self.notice = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let config = block_on(client.save_global_config(&yaml_text, revision))?;
                    // Keys are written only after their connections exist in the
                    // saved configuration (mirrors `prepareCommit`).
                    let saved: std::collections::HashSet<String> = config
                        .effective
                        .get("llm")
                        .and_then(|l| l.get("providers"))
                        .and_then(|p| p.as_object())
                        .map(|m| m.keys().cloned().collect())
                        .unwrap_or_default();
                    let mut keys_failed = false;
                    let mut committed = Vec::new();
                    for (conn, secret) in pending_creds {
                        if !saved.contains(&conn) {
                            continue;
                        }
                        let body = serde_json::json!({"mode": "manual", "secret": secret});
                        if block_on(client.save_credential(&conn, body)).is_ok() {
                            committed.push(conn);
                        } else {
                            keys_failed = true;
                        }
                    }
                    Ok::<_, crate::client::Error>((config, committed, keys_failed))
                })
                .await;
            _ = this.update(cx, |view: &mut GlobalSettingsView, cx| {
                view.op = None;
                match result {
                    Ok((config, committed, keys_failed)) => {
                        for conn in committed {
                            view.cred_drafts.remove(&conn);
                        }
                        view.apply_config(config);
                        view.uncommitted = false;
                        view.refresh_creds(cx);
                        if keys_failed {
                            view.error = Some(
                                "Configuration saved, but an API key could not be saved. Save again to retry.".into(),
                            );
                        } else {
                            view.notice = Some("Global settings saved".into());
                        }
                    }
                    Err(crate::client::Error::Api { status: 409, .. }) => {
                        view.error = Some(
                            "Settings changed elsewhere (revision conflict). Reloaded the latest version.".into(),
                        );
                        view.refresh(cx);
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
        let Some(client) = self.client.clone() else {
            return;
        };
        let yaml_text = self.yaml.read(cx).value().to_string();
        self.op = Some("validate");
        self.error = None;
        self.notice = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { block_on(client.validate_global_config(&yaml_text)) })
                .await;
            _ = this.update(cx, |view: &mut GlobalSettingsView, cx| {
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
                        view.yaml_dirty = false;
                        view.typing_int = None;
                        view.uncommitted = true;
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
        let Some(client) = self.client.clone() else {
            return;
        };
        let revision = self.revision;
        self.op = Some("restore");
        self.error = None;
        self.notice = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { block_on(client.get_global_defaults()) })
                .await;
            _ = this.update(cx, |view: &mut GlobalSettingsView, cx| {
                view.op = None;
                match result {
                    Ok(mut config) => {
                        config.revision = revision;
                        view.apply_config(config);
                        view.uncommitted = true;
                        view.cred_drafts.clear();
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

    fn refresh_creds(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { block_on(client.credential_statuses()) })
                .await;
            _ = this.update(cx, |view: &mut GlobalSettingsView, cx| {
                if let Ok(creds) = result {
                    view.cred_statuses = creds;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    // -- MinerU autosave (debounce 800ms + blur) --

    fn on_mineru_changed(&mut self, cx: &mut Context<Self>) {
        self.mineru_dirty = true;
        self.mineru_gen += 1;
        let gen = self.mineru_gen;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(800))
                .await;
            _ = this.update(cx, |view: &mut GlobalSettingsView, cx| {
                if view.mineru_gen == gen && view.mineru_dirty {
                    view.save_mineru_now(cx);
                }
            });
        })
        .detach();
    }

    fn save_mineru_now(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if !self.mineru_dirty || self.mineru_saving {
            return;
        }
        let secret = self.mineru_input.read(cx).value().to_string();
        self.mineru_saving = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let body = if secret.trim().is_empty() {
                serde_json::json!({"clear": true})
            } else {
                serde_json::json!({"secret": secret})
            };
            let result = cx
                .background_spawn(async move { block_on(client.save_mineru(body)) })
                .await;
            _ = this.update(cx, |view: &mut GlobalSettingsView, cx| {
                view.mineru_saving = false;
                match result {
                    Ok(status) => {
                        view.mineru_status = status;
                        view.mineru_dirty = false;
                        view.mineru_pending = Some(String::new());
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

    // -- Model name suggestions --

    fn fetch_suggestions(&mut self, model_id: &str, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let models: HashMap<String, serde_json::Value> =
            self.models().into_iter().collect();
        let Some(model) = models.get(model_id).cloned() else {
            return;
        };
        let conn = model
            .get("provider")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let providers: HashMap<String, serde_json::Value> =
            self.providers().into_iter().collect();
        let provider = providers.get(&conn).cloned().unwrap_or(serde_json::Value::Null);
        let api_key = self
            .cred_drafts
            .get(&conn)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let mut body = serde_json::json!({"provider": provider, "timeout": 20.0});
        if !conn.is_empty() {
            body["connection"] = serde_json::Value::from(conn.clone());
        }
        if let Some(key) = api_key {
            body["api_key"] = serde_json::Value::from(key);
        }
        let key = model_id.to_string();
        self.fetching_models.insert(key.clone());
        self.suggestion_error.remove(&key);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { block_on(client.list_models_remote(body)) })
                .await;
            _ = this.update(cx, |view: &mut GlobalSettingsView, cx| {
                view.fetching_models.remove(&key);
                match result {
                    Ok(value) => {
                        let names: Vec<String> = value
                            .get("models")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()
                            })
                            .unwrap_or_default();
                        if value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
                            view.suggestions.insert(key, names);
                        } else {
                            let msg = value
                                .get("message")
                                .and_then(|v| v.as_str())
                                .unwrap_or("This provider cannot list models. Enter the name.");
                            view.suggestion_error.insert(key, msg.to_string());
                        }
                    }
                    Err(err) => {
                        view.suggestion_error.insert(key, err.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // -- Connectivity ping --

    fn do_ping(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if !self.config_saved() || self.dirty() {
            return;
        }
        self.pinging = true;
        self.ping_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { block_on(client.ping_models(30.0)) })
                .await;
            _ = this.update(cx, |view: &mut GlobalSettingsView, cx| {
                view.pinging = false;
                match result {
                    Ok(results) => {
                        view.ping_results = Some(results);
                    }
                    Err(err) => {
                        view.ping_error = Some(err.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // -- Widget helpers --

    fn text_input(&mut self, key: &str, initial: String, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some(entity) = self.inputs.get(key) {
            return entity.clone();
        }
        let entity = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(initial, window, cx);
            state
        });
        let owned = key.to_string();
        cx.subscribe(&entity, move |this: &mut GlobalSettingsView, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.on_text_changed(&owned, cx);
            }
        })
        .detach();
        self.inputs.insert(key.to_string(), entity.clone());
        entity
    }

    fn cred_input(&mut self, conn: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let key = format!("cred:{conn}");
        if let Some(entity) = self.inputs.get(&key) {
            return entity.clone();
        }
        let entity = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("API key (saved keys stay hidden)"));
        let conn_owned = conn.to_string();
        cx.subscribe(&entity, move |this: &mut GlobalSettingsView, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.on_cred_changed(&conn_owned, cx);
            }
        })
        .detach();
        self.inputs.insert(key, entity.clone());
        entity
    }

    fn rename_input(&mut self, group: &str, id: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        let key = format!("rename:{group}:{id}");
        if let Some(entity) = self.rename_inputs.get(&key) {
            return entity.clone();
        }
        let entity = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(id.to_string(), window, cx);
            state
        });
        let (g, old) = (group.to_string(), id.to_string());
        cx.subscribe(&entity, move |this: &mut GlobalSettingsView, _, event: &InputEvent, cx| {
            match event {
                InputEvent::Blur => this.apply_rename_input(&g, &old, cx),
                InputEvent::Change => {
                    let k = format!("rename:{g}:{old}");
                    if let Some(e) = this.rename_inputs.get(&k).cloned() {
                        let v = e.read(cx).value().to_string();
                        if v != old {
                            this.editing_ids.insert(format!("{g}:{old}"));
                        } else {
                            this.editing_ids.remove(&format!("{g}:{old}"));
                        }
                        cx.notify();
                    }
                }
                _ => {}
            }
        })
        .detach();
        self.rename_inputs.insert(key, entity.clone());
        entity
    }

    fn apply_rename_input(&mut self, group: &str, old_id: &str, cx: &mut Context<Self>) {
        let key = format!("rename:{group}:{old_id}");
        let Some(entity) = self.rename_inputs.get(&key).cloned() else {
            return;
        };
        let next = entity.read(cx).value().to_string();
        if next.trim() == old_id {
            self.editing_ids.remove(&format!("{group}:{old_id}"));
            cx.notify();
            return;
        }
        self.commit_rename(group, old_id, &next, cx);
    }

    fn on_text_changed(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.form_disabled() {
            return;
        }
        let Some(entity) = self.inputs.get(key).cloned() else {
            return;
        };
        let value = entity.read(cx).value().to_string();
        let parts: Vec<&str> = key.splitn(3, ':').collect();
        if parts.len() != 3 {
            return;
        }
        let (scope, id, field) = (parts[0], parts[1], parts[2]);
        let group = if scope == "p" { "providers" } else { "models" };
        let json = match field {
            "timeout" => match value.trim().parse::<i64>() {
                Ok(v) if v >= 1 => serde_json::Value::from(v),
                _ => return,
            },
            "base_url" | "api_key_env" => {
                if value.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::from(value.clone())
                }
            }
            "name" => serde_json::Value::from(value.clone()),
            _ => return,
        };
        // Keep provider kind changes on the select path; text fields here.
        self.update_registry(group, id, field, json, cx);
    }

    fn on_cred_changed(&mut self, conn: &str, cx: &mut Context<Self>) {
        let key = format!("cred:{conn}");
        let Some(entity) = self.inputs.get(&key).cloned() else {
            return;
        };
        let value = entity.read(cx).value().to_string();
        if value.trim().is_empty() {
            self.cred_drafts.remove(conn);
        } else {
            self.cred_drafts.insert(conn.to_string(), value);
        }
        cx.notify();
    }

    /// Lazily build (or rebuild when options change) a select; Confirm routes
    /// to `on_select` by key.
    fn ensure_select(
        &mut self,
        key: &str,
        opts: Vec<String>,
        current: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<SelectState<Vec<String>>> {
        let sig = opts.join("|");
        if self.select_sigs.get(key).map(|s| s != &sig).unwrap_or(false) {
            self.selects.remove(key);
        }
        if let Some(entity) = self.selects.get(key) {
            return entity.clone();
        }
        let idx = opts.iter().position(|o| o == current).unwrap_or(0);
        let entity = cx.new(|cx| SelectState::new(opts, Some(IndexPath::new(idx)), window, cx));
        let owned = key.to_string();
        cx.subscribe(
            &entity,
            move |this: &mut GlobalSettingsView, _, event: &SelectEvent<Vec<String>>, cx| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    let value = value.clone();
                    this.on_select(&owned, &value, cx);
                }
            },
        )
        .detach();
        self.select_sigs.insert(key.to_string(), sig);
        self.selects.insert(key.to_string(), entity.clone());
        entity
    }

    fn on_select(&mut self, key: &str, value: &str, cx: &mut Context<Self>) {
        if key == "lang" {
            self.language = value.to_string();
            cx.notify();
            return;
        }
        if self.form_disabled() {
            return;
        }
        if key == "pdf_backend" {
            self.pdf_backend = value.to_string();
            self.set_field("pipeline", "pdf_backend", serde_json::Value::from(value), cx);
            return;
        }
        if let Some(tier) = key.strip_prefix("tier:") {
            self.tier_values.insert(tier.to_string(), value.to_string());
            let mut tiers = section(&llm(&self.effective), "tiers");
            if let Some(map) = tiers.as_object_mut() {
                map.insert(tier.to_string(), serde_json::Value::from(value));
            }
            let mut next_llm = llm(&self.effective);
            if let Some(map) = next_llm.as_object_mut() {
                map.insert("preset".to_string(), serde_json::Value::Null);
                map.insert("tiers".to_string(), tiers);
            }
            self.set_llm(next_llm, cx);
            return;
        }
        if let Some(op) = key.strip_prefix("op:") {
            self.op_values.insert(op.to_string(), value.to_string());
            let mut routes = section(&llm(&self.effective), "routes");
            if !routes.is_object() {
                routes = serde_json::Value::Object(Default::default());
            }
            let current = routes.get(op).cloned().unwrap_or(serde_json::Value::Null);
            let fallbacks = current
                .get("fallbacks")
                .cloned()
                .unwrap_or_else(|| serde_json::Value::from(Vec::<serde_json::Value>::new()));
            let tier_default = self
                .operations
                .iter()
                .find(|o| o.get("id").and_then(|v| v.as_str()) == Some(op))
                .and_then(|o| o.get("tier"))
                .and_then(|v| v.as_str())
                .unwrap_or("strong");
            let next = if value == format!("tier:{tier_default}") && fallbacks.as_array().map(|a| a.is_empty()).unwrap_or(true) {
                serde_json::Value::Null
            } else if let Some(tier) = value.strip_prefix("tier:") {
                serde_json::json!({"tier": tier, "fallbacks": fallbacks})
            } else {
                serde_json::json!({"model": value, "fallbacks": fallbacks})
            };
            if let Some(map) = routes.as_object_mut() {
                if next.is_null() {
                    map.remove(op);
                } else {
                    map.insert(op.to_string(), next);
                }
            }
            let mut next_llm = llm(&self.effective);
            if let Some(map) = next_llm.as_object_mut() {
                map.insert("preset".to_string(), serde_json::Value::Null);
                map.insert("routes".to_string(), routes);
            }
            self.set_llm(next_llm, cx);
            return;
        }
        if let Some(rest) = key.strip_prefix("kind:") {
            // Protocol-specific options cannot be carried to another adapter.
            let l = llm(&self.effective);
            let providers = l.get("providers").cloned().unwrap_or(serde_json::Value::Null);
            let models = l.get("models").cloned().unwrap_or(serde_json::Value::Null);
            let mut next_llm = l.clone();
            if let Some(map) = next_llm.as_object_mut() {
                let mut pmap = providers.as_object().cloned().unwrap_or_default();
                pmap.insert(
                    rest.to_string(),
                    serde_json::json!({"kind": value, "base_url": null, "api_key_env": null}),
                );
                map.insert("providers".to_string(), serde_json::Value::Object(pmap));
                let mut mmap = models.as_object().cloned().unwrap_or_default();
                for (_, m) in mmap.iter_mut() {
                    if m.get("provider").and_then(|v| v.as_str()) == Some(rest) {
                        if let Some(mm) = m.as_object_mut() {
                            mm.insert("options".to_string(), serde_json::json!({}));
                        }
                    }
                }
                map.insert("models".to_string(), serde_json::Value::Object(mmap));
                map.insert("preset".to_string(), serde_json::Value::Null);
            }
            self.set_llm(next_llm, cx);
            return;
        }
        if let Some(rest) = key.strip_prefix("conn:") {
            self.update_registry(
                "models",
                rest,
                "provider",
                serde_json::Value::from(value),
                cx,
            );
            let mut m = self
                .models()
                .into_iter()
                .find(|(id, _)| id == rest)
                .map(|(_, m)| m);
            if let Some(m) = m.take() {
                let _ = m;
            }
            // Reset options when the connection changes (mirrors source).
            self.update_registry("models", rest, "options", serde_json::json!({}), cx);
        }
    }
}

impl Render for GlobalSettingsView {
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
        if let Some(text) = self.mineru_pending.take() {
            if self.mineru_input.read(cx).value().to_string() != text {
                self.mineru_input.update(cx, |state, cx| {
                    state.set_value(text, window, cx);
                });
            }
        }
        for (key, text) in std::mem::take(&mut self.pending_text) {
            if let Some(entity) = self.inputs.get(&key).cloned() {
                if entity.read(cx).value().to_string() != text {
                    entity.update(cx, |state, cx| {
                        state.set_value(text, window, cx);
                    });
                }
            }
        }
        for (key, text) in std::mem::take(&mut self.rename_pending) {
            if let Some(entity) = self.rename_inputs.get(&key).cloned() {
                if entity.read(cx).value().to_string() != text {
                    entity.update(cx, |state, cx| {
                        state.set_value(text, window, cx);
                    });
                }
            }
        }

        // Language select (fixed options).
        if self.lang_select.is_none() {
            let opts: Vec<String> = LOCALES.iter().map(|(c, _)| c.to_string()).collect();
            let idx = opts.iter().position(|o| o == &self.language).unwrap_or(0);
            let entity = cx.new(|cx| SelectState::new(opts, Some(IndexPath::new(idx)), window, cx));
            cx.subscribe(
                &entity,
                |this: &mut GlobalSettingsView, _, event: &SelectEvent<Vec<String>>, cx| {
                    if let SelectEvent::Confirm(Some(value)) = event {
                        this.language = value.clone();
                        cx.notify();
                    }
                },
            )
            .detach();
            self.lang_select = Some(entity);
        }
        if self.theme_mode_select.is_none() {
            let opts = vec!["system".to_string(), "dark".to_string(), "light".to_string(), "noctalia".to_string()];
            let cur = match self.theme_choice.mode {
                theme::ThemeMode::System => "system",
                theme::ThemeMode::Dark => "dark",
                theme::ThemeMode::Light => "light",
                theme::ThemeMode::Noctalia => "noctalia",
            };
            let idx = opts.iter().position(|o| o == cur).unwrap_or(0);
            let entity = cx.new(|cx| SelectState::new(opts, Some(IndexPath::new(idx)), window, cx));
            cx.subscribe(&entity, |this, _, event: &SelectEvent<Vec<String>>, cx| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    this.theme_choice.mode = match value.as_str() {
                        "dark" => theme::ThemeMode::Dark,
                        "light" => theme::ThemeMode::Light,
                        "noctalia" => theme::ThemeMode::Noctalia,
                        _ => theme::ThemeMode::System,
                    };
                    let _ = theme::save_choice(&this.theme_choice);
                    theme::set_choice(this.theme_choice);
                    if let Some(h) = &this.app_handle {
                        let choice = this.theme_choice;
                        let _ = h.update(cx, |app, cx| { app.theme_choice = choice; cx.notify(); });
                    }
                    cx.notify();
                }
            }).detach();
            self.theme_mode_select = Some(entity);
        }
        if self.theme_accent_select.is_none() {
            let opts: Vec<String> = theme::Accent::all().iter().map(|a| a.name().to_string()).collect();
            let cur = self.theme_choice.accent.name().to_string();
            let idx = opts.iter().position(|o| o == &cur).unwrap_or(0);
            let entity = cx.new(|cx| SelectState::new(opts, Some(IndexPath::new(idx)), window, cx));
            cx.subscribe(&entity, |this, _, event: &SelectEvent<Vec<String>>, cx| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    if let Some(a) = theme::Accent::all().iter().find(|a| a.name() == value) {
                        this.theme_choice.accent = *a;
                    }
                    let _ = theme::save_choice(&this.theme_choice);
                    theme::set_choice(this.theme_choice);
                    if let Some(h) = &this.app_handle {
                        let choice = this.theme_choice;
                        let _ = h.update(cx, |app, cx| { app.theme_choice = choice; cx.notify(); });
                    }
                    cx.notify();
                }
            }).detach();
            self.theme_accent_select = Some(entity);
        }
        let form_disabled = self.form_disabled();
        let op_busy = self.op.is_some();
        let dirty = self.dirty();
        let config_saved = self.config_saved();

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
            .child(div().text_xl().font_weight(FontWeight::BOLD).child("Settings"));

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

        // -- General / language --
        col = col.child(
            div().flex().flex_col().gap_2().child(
                div().text_lg().font_weight(FontWeight::BOLD).child("General"),
            ).child(
                div().flex().flex_col().gap_1().child("Interface language").child(
                    Select::new(self.lang_select.as_ref().unwrap()),
                ),
            ),
        );
        col = col.child(
            div().flex().flex_col().gap_2().child(
                div().text_lg().font_weight(FontWeight::BOLD).child("Theme"),
            ).child(
                div().flex().flex_col().gap_1().child("Mode").child(
                    Select::new(self.theme_mode_select.as_ref().unwrap()),
                ),
            ).child(
                div().flex().flex_col().gap_1().child("Accent").child(
                    Select::new(self.theme_accent_select.as_ref().unwrap()),
                ),
            ),
        );

        // -- Providers & models --
        {
            let mut body = div().flex().flex_col().gap_2().child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_lg().font_weight(FontWeight::BOLD).child("Providers & models"))
                    .child(
                        Button::new("add-provider")
                            .label("Add API connection")
                            .disabled(form_disabled)
                            .on_click(cx.listener(|this, _, _, cx| this.add_entry("providers", cx))),
                    ),
            );
            for (id, provider) in self.providers() {
                let refs = registry_references(&self.effective, "providers", &id);
                let kind = provider.get("kind").and_then(|v| v.as_str()).unwrap_or("openai-compatible").to_string();
                let mut kind_opts = vec![kind.clone()];
                for k in &self.provider_kinds {
                    if Some(k) != Some(&kind) {
                        kind_opts.push(k.clone());
                    }
                }
                let kind_select = self.ensure_select(&format!("kind:{id}"), kind_opts, &kind, window, cx);
                let base_url = provider.get("base_url").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let api_key_env = provider.get("api_key_env").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let timeout = provider.get("timeout").and_then(|v| v.as_i64()).unwrap_or(600).to_string();
                let url_input = self.text_input(&format!("p:{id}:base_url"), base_url, window, cx);
                let env_input = self.text_input(&format!("p:{id}:api_key_env"), api_key_env, window, cx);
                let timeout_input = self.text_input(&format!("p:{id}:timeout"), timeout, window, cx);
                let cred = self.cred_input(&id, window, cx);
                let rename = self.rename_input("providers", &id, window, cx);
                let cred_available = self
                    .cred_statuses
                    .get(&id)
                    .and_then(|s| s.get("available"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let cred_mode = self
                    .cred_statuses
                    .get(&id)
                    .and_then(|s| s.get("mode"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("environment")
                    .to_string();
                let cred_pending = self.cred_drafts.get(&id).map(|v| !v.trim().is_empty()).unwrap_or(false);
                let can_delete = refs.is_empty();
                let id_clone = id.clone();
                let mut card = div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(palette.border)
                    .child(
                        div().flex().gap_2().items_center()
                            .child(div().flex_1().flex().flex_col().gap_1().child("Connection ID").child(Input::new(&rename).disabled(form_disabled)))
                            .child(
                                Button::new(format!("del-provider-{id}"))
                                    .label("Delete")
                                    .disabled(form_disabled || !can_delete)
                                    .on_click(cx.listener(move |this, _, _, cx| this.remove_entry("providers", &id_clone, cx))),
                            ),
                    );
                if !can_delete {
                    card = card.child(
                        div().text_sm().text_color(palette.muted_foreground)
                            .child(format!("Used by: {}", refs.join(", "))),
                    );
                }
                card = card
                    .child(
                        div().flex().flex_col().gap_1().child("API provider").child(
                            Select::new(&kind_select).disabled(form_disabled),
                        ),
                    )
                    .child(
                        div().flex().flex_col().gap_1()
                            .child("API base URL (leave blank for the provider default URL)")
                            .child(Input::new(&url_input).disabled(form_disabled)),
                    )
                    .child(
                        div().flex().flex_col().gap_1()
                            .child("API key environment variable (leave blank for the provider default variable)")
                            .child(Input::new(&env_input).disabled(form_disabled)),
                    )
                    .child(
                        div().flex().flex_col().gap_1()
                            .child("Request timeout (seconds)")
                            .child(Input::new(&timeout_input).disabled(form_disabled)),
                    )
                    .child(
                        div().flex().flex_col().gap_1()
                            .child(format!(
                                "API key{}",
                                if cred_available { format!(" (saved via {cred_mode})") } else { " (not set)".to_string() }
                            ))
                            .child(Input::new(&cred).disabled(form_disabled))
                            .child(
                                div().text_sm().text_color(palette.muted_foreground).child(
                                    if cred_pending {
                                        "Unsaved key — it is written after the configuration saves."
                                    } else {
                                        "Saved keys stay hidden; typing a key stages it for the next save."
                                    },
                                ),
                            ),
                    );
                body = body.child(card);
            }
            // Models
            {
                let mut mbody = div().flex().flex_col().gap_2().child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().text_lg().font_weight(FontWeight::BOLD).child("Models"))
                        .child(
                            Button::new("add-model")
                                .label("Add model")
                                .disabled(form_disabled || self.providers().is_empty())
                                .on_click(cx.listener(|this, _, _, cx| this.add_entry("models", cx))),
                        ),
                );
                let provider_ids: Vec<String> = self.providers().into_iter().map(|(id, _)| id).collect();
                for (id, model) in self.models() {
                    let conn = model.get("provider").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let name = model.get("model").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let conn_select = self.ensure_select(&format!("conn:{id}"), provider_ids.clone(), &conn, window, cx);
                    let name_input = self.text_input(&format!("m:{id}:name"), name, window, cx);
                    let rename = self.rename_input("models", &id, window, cx);
                    let refs = registry_references(&self.effective, "models", &id);
                    let can_delete = refs.is_empty();
                    let id_clone = id.clone();
                    let id_fetch = id.clone();
                    let mut card = div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .p_3()
                        .rounded_md()
                        .border_1()
                        .border_color(palette.border)
                        .child(
                            div().flex().gap_2().items_center()
                                .child(div().flex_1().flex().flex_col().gap_1().child("Model ID").child(Input::new(&rename).disabled(form_disabled)))
                                .child(
                                    Button::new(format!("del-model-{id}"))
                                        .label("Delete")
                                        .disabled(form_disabled || !can_delete)
                                        .on_click(cx.listener(move |this, _, _, cx| this.remove_entry("models", &id_clone, cx))),
                                ),
                        );
                    if !can_delete {
                        card = card.child(
                            div().text_sm().text_color(palette.muted_foreground)
                                .child(format!("Used by: {}", refs.join(", "))),
                        );
                    }
                    card = card
                        .child(
                            div().flex().flex_col().gap_1().child("API connection").child(
                                Select::new(&conn_select).disabled(form_disabled),
                            ),
                        )
                        .child(
                            div().flex().flex_col().gap_1().child("Model name").child(Input::new(&name_input).disabled(form_disabled)),
                        );
                    // Suggestions (mirrors ModelNameField fetch).
                    if self.fetching_models.contains(&id) {
                        card = card.child(div().text_sm().text_color(palette.muted_foreground).child("Loading model names…"));
                    } else if let Some(names) = self.suggestions.get(&id) {
                        if names.is_empty() {
                            card = card.child(div().text_sm().text_color(palette.muted_foreground).child("No models returned. Enter the name."));
                        } else {
                            let mut list = div().flex().flex_col().gap_1();
                            for n in names.iter().take(20) {
                                list = list.child(div().text_sm().child(n.clone()));
                            }
                            card = card.child(list);
                        }
                    } else if let Some(err) = self.suggestion_error.get(&id) {
                        card = card.child(div().text_sm().text_color(palette.muted_foreground).child(err.clone()));
                    }
                    card = card.child(
                        Button::new(format!("fetch-models-{id}"))
                            .label("Fetch model names")
                            .disabled(form_disabled)
                            .on_click(cx.listener(move |this, _, _, cx| this.fetch_suggestions(&id_fetch, cx))),
                    );
                    mbody = mbody.child(card);
                }
                body = body.child(mbody);
            }
            // Connectivity (mirrors ModelConnectivity; saved-only gate).
            {
                let mut cbody = div().flex().flex_col().gap_2().child(
                    div().flex().gap_2().items_center().child(
                        Button::new("ping")
                            .label(if self.pinging { "Testing…" } else { "Test connection" })
                            .disabled(form_disabled || self.pinging || !config_saved || dirty)
                            .on_click(cx.listener(|this, _, _, cx| this.do_ping(cx))),
                    ).child(
                        div().text_sm().text_color(palette.muted_foreground).child(
                            if !config_saved || dirty {
                                "Save the configuration before testing the connection."
                            } else {
                                ""
                            },
                        ),
                    ),
                );
                if let Some(err) = &self.ping_error {
                    cbody = cbody.child(
                        div().p_3().rounded_md().bg(palette.destructive).text_color(palette.destructive_foreground).child(format!("Error: {err}")),
                    );
                }
                if let Some(results) = &self.ping_results {
                    if results.is_empty() {
                        cbody = cbody.child(div().text_sm().child("No models are configured."));
                    } else {
                        let failed = results.iter().filter(|r| !r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false)).count();
                        cbody = cbody.child(div().text_sm().child(
                            if failed == 0 {
                                format!("All {} models are reachable.", results.len())
                            } else {
                                format!("{failed} of {} models are unreachable.", results.len())
                            },
                        ));
                        for r in results {
                            let ok = r.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                            let profile = r.get("profile").and_then(|v| v.as_str()).unwrap_or("?");
                            let model = r.get("model").and_then(|v| v.as_str()).unwrap_or("");
                            let conn = r.get("connection").and_then(|v| v.as_str()).unwrap_or("?");
                            let prov = r.get("provider").and_then(|v| v.as_str()).unwrap_or("?");
                            let latency = r.get("latency_ms").and_then(|v| v.as_i64()).map(|ms| format!("{ms} ms")).unwrap_or_else(|| "—".into());
                            let category = r.get("category").and_then(|v| v.as_str()).unwrap_or("");
                            let message = r.get("message").and_then(|v| v.as_str()).unwrap_or("");
                            let detail = {
                                let base = category_message(category);
                                let text = if base == "Probe failed." && !message.is_empty() { message } else { base };
                                match r.get("status_code").and_then(|v| v.as_i64()) {
                                    Some(code) => format!("{text} (HTTP {code})"),
                                    None => text.to_string(),
                                }
                            };
                            cbody = cbody.child(
                                div().flex().flex_col().gap_1().p_2().rounded_md().border_1().border_color(palette.border)
                                    .child(div().text_sm().child(format!("{} · {model} ({conn} / {prov})", profile)))
                                    .child(div().text_sm().child(format!("{} · {latency} · {detail}", if ok { "Connected" } else { "Failed" }))),
                            );
                        }
                    }
                }
                body = body.child(cbody);
            }
            col = col.child(body);
        }

        // -- Default models (tiers + per-operation routes) --
        {
            let mut body = div().flex().flex_col().gap_2().child(
                div().text_lg().font_weight(FontWeight::BOLD).child("Default models"),
            );
            let model_ids: Vec<String> = self.models().into_iter().map(|(id, _)| id).collect();
            {
                let mut tiers = div().flex().gap_2();
                for (id, label) in TIERS {
                    let current = self.tier_values.get(*id).cloned().unwrap_or_default();
                    let mut opts = Vec::new();
                    if !current.is_empty() && !model_ids.contains(&current) {
                        opts.push(current.clone());
                    }
                    opts.extend(model_ids.clone());
                    let select = self.ensure_select(&format!("tier:{id}"), opts, &current, window, cx);
                    tiers = tiers.child(
                        div().flex_1().flex().flex_col().gap_1().child(*label).child(
                            Select::new(&select).disabled(form_disabled),
                        ),
                    );
                }
                body = body.child(tiers);
            }
            {
                let mut ops = div().flex().flex_col().gap_2().child(
                    div().text_sm().font_medium().child("Models by operation"),
                );
                for op in self.operations.clone() {
                    let oid = op.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    if oid.is_empty() {
                        continue;
                    }
                    let desc = op.get("description").and_then(|v| v.as_str()).unwrap_or(&oid).to_string();
                    let tier_default = op.get("tier").and_then(|v| v.as_str()).unwrap_or("strong");
                    let current = self.op_values.get(&oid).cloned().unwrap_or_else(|| format!("tier:{tier_default}"));
                    let mut opts: Vec<String> = TIERS.iter().map(|(t, _)| format!("tier:{t}")).collect();
                    for m in &model_ids {
                        if !opts.contains(m) {
                            opts.push(m.clone());
                        }
                    }
                    if !opts.contains(&current) {
                        opts.push(current.clone());
                    }
                    let select = self.ensure_select(&format!("op:{oid}"), opts, &current, window, cx);
                    let route = section(&llm(&self.effective), "routes");
                    let fallbacks: Vec<String> = route
                        .get(&oid)
                        .and_then(|r| r.get("fallbacks"))
                        .and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|f| f.as_str().map(|s| s.to_string())).collect())
                        .unwrap_or_default();
                    ops = ops.child(
                        div().flex().flex_col().gap_1().p_2().rounded_md().border_1().border_color(palette.border)
                            .child(div().text_sm().child(desc))
                            .child(Select::new(&select).disabled(form_disabled))
                            .child(div().text_sm().text_color(palette.muted_foreground).child(
                                if fallbacks.is_empty() { "Fallback models: none".to_string() } else { format!("Fallback models: {}", fallbacks.join(", ")) },
                            )),
                    );
                }
                body = body.child(ops);
            }
            col = col.child(body);
        }

        // -- Workflow (global scope + pdf; no translation_mode) --
        {
            let pipeline_book = get_bool(&self.effective, "pipeline", "book_understanding");
            let pipeline_polish = get_bool(&self.effective, "pipeline", "polish");
            let pipeline_review = get_bool(&self.effective, "pipeline", "review");
            let pipeline_autofix = get_bool(&self.effective, "pipeline", "review_autofix");
            let annotation = get_bool(&self.effective, "pipeline", "annotation_alignment");
            let pdf_current = self.pdf_backend.clone();
            let pdf_select = self.ensure_select("pdf_backend", vec!["mineru".to_string(), "babeldoc".to_string()], &pdf_current, window, cx);
            col = col.child(
                div().flex().flex_col().gap_2().child(
                    div().text_lg().font_weight(FontWeight::BOLD).child("Workflow"),
                ).child(
                    Checkbox::new("wf-book").label("Book understanding").checked(pipeline_book).on_change(cx.listener(|this, checked: &bool, _, cx| this.toggle_pipeline("book_understanding", *checked, cx))),
                ).child(
                    Checkbox::new("wf-polish").label("Polishing").checked(pipeline_polish).on_change(cx.listener(|this, checked: &bool, _, cx| this.toggle_pipeline("polish", *checked, cx))),
                ).child(
                    Checkbox::new("wf-review").label("Whole-book review").checked(pipeline_review).on_change(cx.listener(|this, checked: &bool, _, cx| this.toggle_pipeline("review", *checked, cx))),
                ).child(
                    Checkbox::new("wf-autofix").label("Apply autofixes to the saved translation after review").checked(pipeline_autofix).on_change(cx.listener(|this, checked: &bool, _, cx| this.toggle_pipeline("review_autofix", *checked, cx))),
                ).child(
                    div().text_sm().font_medium().child("Segmentation and performance"),
                ).child(
                    Checkbox::new("wf-align").label("Paragraph annotation alignment").checked(annotation).on_change(cx.listener(|this, checked: &bool, _, cx| this.toggle_pipeline("annotation_alignment", *checked, cx))),
                ).child(
                    div().flex().gap_2()
                        .child(div().flex_1().flex().flex_col().gap_1().child("Tokens per batch").child(Input::new(&self.batch_tokens).disabled(form_disabled)))
                        .child(div().flex_1().flex().flex_col().gap_1().child("Tokens per paragraph").child(Input::new(&self.segment_tokens).disabled(form_disabled))),
                ).child(
                    div().flex().gap_2()
                        .child(div().flex_1().flex().flex_col().gap_1().child("Review concurrency").child(Input::new(&self.review_concurrency).disabled(form_disabled)))
                        .child(div().flex_1().flex().flex_col().gap_1().child("PDF parser").child(Select::new(&pdf_select).disabled(form_disabled))),
                ),
            );
        }

        // -- MinerU --
        {
            let mineru_available = self
                .mineru_status
                .get("available")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            col = col.child(
                div().flex().flex_col().gap_2().child(
                    div().text_lg().font_weight(FontWeight::BOLD).child("MinerU PDF parsing"),
                ).child(
                    div().flex().flex_col().gap_1()
                        .child(format!("MinerU API key{}", if mineru_available { " (saved)" } else { " (not set)" }))
                        .child(Input::new(&self.mineru_input).disabled(self.mineru_saving))
                        .child(div().text_sm().text_color(palette.muted_foreground).child(
                            if self.mineru_saving { "Saving…" } else { "Saved automatically after you stop typing." },
                        )),
                ),
            );
        }

        // -- Advanced YAML --
        {
            col = col.child(
                div().flex().flex_col().gap_2().child(
                    div().text_lg().font_weight(FontWeight::BOLD).child("Advanced"),
                ).child(
                    div().flex().flex_col().gap_1()
                        .child("Advanced YAML configuration")
                        .child(Textarea::new(&self.yaml)),
                ),
            );
            if self.yaml_dirty {
                col = col.child(
                    div().text_sm().text_color(palette.muted_foreground)
                        .child("Advanced YAML has unvalidated changes. Validate it to update the form."),
                );
            }
        }

        // -- Action bar --
        {
            let save_label = if op_busy { "Saving…" } else { "Save configuration" };
            col = col.child(
                div().flex().gap_2().items_center()
                    .child(
                        Button::new("restore")
                            .ghost()
                            .label("Restore defaults")
                            .disabled(self.loaded == false || op_busy)
                            .on_click(cx.listener(|this, _, _, cx| this.do_restore(cx))),
                    )
                    .child(
                        Button::new("validate")
                            .label("Validate configuration")
                            .disabled(self.loaded == false || op_busy || !self.editing_ids.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| this.do_validate(cx))),
                    )
                    .child(
                        Button::new("save")
                            .label(save_label)
                            .primary()
                            .disabled(self.loaded == false || op_busy || !self.editing_ids.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| this.do_save(cx))),
                    )
                    .child(
                        div().text_sm().text_color(palette.muted_foreground).child(
                            if !self.editing_ids.is_empty() {
                                "Apply or discard the ID edits before saving."
                            } else if dirty {
                                "Unsaved changes"
                            } else {
                                ""
                            },
                        ),
                    ),
            );
        }

        // -- Updater (not ported) --
        col = col.child(
            div().flex().flex_col().gap_1().p_3().rounded_md().border_1().border_color(palette.border)
                .child(div().text_sm().font_medium().child("App updates"))
                .child(div().text_sm().text_color(palette.muted_foreground).child("The Tauri auto-updater section is not ported to the native app yet.")),
        );

        col
    }
}
