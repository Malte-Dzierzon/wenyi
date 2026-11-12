//! Global settings (screen 13): language, providers & models, credential
//! keys, connectivity ping, model selection + suggestions, routing,
//! workflow, advanced YAML. Load/save via `get_global_config` /
//! `save_global_config` (last-write-wins). Missing engine pieces
//! (operations caps, mineru, validate, defaults, revision-409) render as
//! hints, never crash.

use std::collections::{HashMap, HashSet};

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{Disableable, IndexPath, StyledExt};
use gpui_kit::*;
use gpui_kit::{InteractiveElement, StatefulInteractiveElement};

use crate::{
    service::{
        block_on, CredentialStatus, GlobalConfigOut, ModelPing, ServiceError, ServiceHandle,
    },
    theme,
};
/// Language options: (code, label). Local pref only.
const LOCALES: &[(&str, &str)] = &[("en", "English"), ("zh-CN", "简体中文")];

/// Tier ids in display order.
const TIERS: &[(&str, &str)] = &[
    ("strong", "Quality tier"),
    ("cheap", "Economy tier"),
    ("fast", "Fast tier"),
];

/// Settings workspace tab. Tab switches are render-only (`cx.notify()`);
/// widget entities stay cached globally so hidden tabs keep focus/drafts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
enum SettingsTab {
    #[default]
    General,
    Providers,
    Models,
    Routing,
    Workflow,
    Advanced,
}

impl SettingsTab {
    const ALL: &'static [(SettingsTab, &'static str)] = &[
        (SettingsTab::General, "General"),
        (SettingsTab::Providers, "Providers"),
        (SettingsTab::Models, "Models"),
        (SettingsTab::Routing, "Routing"),
        (SettingsTab::Workflow, "Workflow"),
        (SettingsTab::Advanced, "Advanced"),
    ];
}

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

/// Set `[group][key] = value` on the effective object.
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
    let l = llm(effective);
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

fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Rename `llm[group][old]` to `new`, remapping tiers/routes/provider refs.
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
                            m.insert("provider".to_string(), serde_json::Value::from(new_id));
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

/// Entries referencing `id` (blocks delete until empty).
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

// (Removed: category_message — ModelPing carries only connection/ok/models/error.)

pub struct GlobalSettingsView {
    service: Option<ServiceHandle>,
    loading: bool,
    loaded: bool,
    error: Option<String>,
    notice: Option<String>,
    effective: serde_json::Value,
    saved_effective: serde_json::Value,
    saved_yaml: Option<String>,
    draft_yaml: Option<String>,
    saved_revision: i64,
    provider_kinds: Vec<String>,
    cred_statuses: Vec<CredentialStatus>,
    language: String,
    lang_select: Option<Entity<SelectState<Vec<String>>>>,
    theme_choice: theme::ThemeChoice,
    theme_mode_select: Option<Entity<SelectState<Vec<String>>>>,
    theme_accent_select: Option<Entity<SelectState<Vec<String>>>>,
    app_handle: Option<WeakEntity<crate::views::App>>,
    op: Option<&'static str>,
    inputs: HashMap<String, Entity<InputState>>,
    cred_drafts: HashMap<String, String>,
    rename_inputs: HashMap<String, Entity<InputState>>,
    /// "group:id" keys with an uncommitted ID edit.
    editing_ids: HashSet<String>,
    /// "group:id" keys of expanded registry accordions.
    open_ids: HashSet<String>,
    /// Tabs edited since the last save/load, for the dirty dots.
    touched: HashSet<SettingsTab>,
    selects: HashMap<String, Entity<SelectState<Vec<String>>>>,
    select_sigs: HashMap<String, String>,
    tier_values: HashMap<String, String>,
    op_values: HashMap<String, String>,
    operations: Vec<serde_json::Value>,
    pdf_backend: String,
    yaml_input: Option<Entity<InputState>>,
    suggestions: HashMap<String, Vec<String>>,
    suggestion_error: HashMap<String, String>,
    fetching_models: HashSet<String>,
    ping_results: Option<Vec<ModelPing>>,
    ping_error: Option<String>,
    pinging: bool,
    /// Active workspace tab (General default).
    tab: SettingsTab,
}

impl GlobalSettingsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let _ = window;
        let _ = cx;
        Self {
            service: None,
            loading: true,
            loaded: false,
            error: None,
            notice: None,
            effective: serde_json::Value::Object(Default::default()),
            saved_effective: serde_json::Value::Object(Default::default()),
            saved_yaml: None,
            draft_yaml: None,
            saved_revision: 0,
            provider_kinds: Vec::new(),
            cred_statuses: Vec::new(),
            language: "en".into(),
            theme_choice: theme::load_choice(),
            lang_select: None,
            theme_mode_select: None,
            theme_accent_select: None,
            app_handle: None,
            op: None,
            inputs: HashMap::new(),
            cred_drafts: HashMap::new(),
            rename_inputs: HashMap::new(),
            editing_ids: HashSet::new(),
            open_ids: HashSet::new(),
            touched: HashSet::new(),
            selects: HashMap::new(),
            select_sigs: HashMap::new(),
            tier_values: HashMap::new(),
            op_values: HashMap::new(),
            operations: Vec::new(),
            pdf_backend: "mineru".into(),
            yaml_input: None,
            suggestions: HashMap::new(),
            suggestion_error: HashMap::new(),
            fetching_models: HashSet::new(),
            ping_results: None,
            ping_error: None,
            pinging: false,
            tab: SettingsTab::General,
        }
    }

    pub fn set_client(&mut self, service: ServiceHandle, cx: &mut Context<Self>) {
        self.service = Some(service);
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
        !self.loaded || self.op.is_some()
    }

    fn cred_pending(&self) -> bool {
        self.cred_drafts.values().any(|v| !v.trim().is_empty())
    }
    fn dirty(&self) -> bool {
        self.effective != self.saved_effective || self.cred_pending()
    }

    fn config_saved(&self) -> bool {
        self.effective == self.saved_effective
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(service) = self.service.clone() else {
            return;
        };
        self.loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let config = service.get_global_config()?;
                    let caps = service.capabilities();
                    let creds = service.credential_statuses();
                    Ok::<_, ServiceError>((config, caps, creds))
                })
                .await;
            _ = this.update(cx, |view: &mut GlobalSettingsView, cx| {
                view.loading = false;
                match result {
                    Ok((config, caps, creds)) => {
                        view.apply_config(config);
                        view.provider_kinds = caps
                            .provider_kinds
                            .into_iter()
                            .filter(|k| k != "fake")
                            .collect();
                        view.operations = caps
                            .operations
                            .into_iter()
                            .map(|op| {
                                serde_json::json!({
                                    "id": op.id,
                                    "tier": op.tier,
                                    "description": op.description,
                                })
                            })
                            .collect();
                        view.cred_statuses = creds;
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
        self.saved_revision = config.revision;
        let yaml = config.yaml.clone();
        self.saved_yaml = Some(yaml.clone());
        self.draft_yaml = Some(yaml);
        self.saved_effective = config.effective.clone();
        self.effective = config.effective;
        self.suggestions.clear();
        self.suggestion_error.clear();
        self.ping_results = None;
        self.ping_error = None;
        self.editing_ids.clear();
        self.touched.clear();
        self.sync_tier_widgets();
        self.sync_workflow_widgets();
    }

    fn sync_tier_widgets(&mut self) {
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
        for op in self.operations.clone() {
            let oid = op
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
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
                    format!(
                        "tier:{}",
                        r.get("tier").and_then(|v| v.as_str()).unwrap_or("")
                    )
                });
            self.op_values.insert(oid, value);
        }
    }

    fn sync_workflow_widgets(&mut self) {
        self.pdf_backend = get_str(&self.effective, "pipeline", "pdf_backend", "mineru");
    }

    /// Per-tab dirty dot: edited since last save/load plus live markers.
    /// General is prefs-only and never dirty.
    fn tab_dirty(&self, tab: SettingsTab) -> bool {
        if self.touched.contains(&tab) {
            return true;
        }
        match tab {
            SettingsTab::General => false,
            SettingsTab::Providers => {
                self.editing_ids.iter().any(|k| k.starts_with("providers:")) || self.cred_pending()
            }
            SettingsTab::Models => self.editing_ids.iter().any(|k| k.starts_with("models:")),
            SettingsTab::Routing => false,
            SettingsTab::Workflow => false,
            SettingsTab::Advanced => {
                self.draft_yaml.as_deref().unwrap_or("")
                    != self.saved_yaml.as_deref().unwrap_or("\u{0}")
            }
        }
    }

    fn touch(&mut self, tab: SettingsTab) {
        self.touched.insert(tab);
    }

    /// Mirror of React `setField`: update effective + YAML draft.
    fn set_field(
        &mut self,
        group: &str,
        key: &str,
        value: serde_json::Value,
        cx: &mut Context<Self>,
    ) {
        self.effective = workflow_field(&self.effective, group, key, value);
        self.touch(SettingsTab::Workflow);
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
        self.draft_yaml = Some(draft);
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
        cx.notify();
    }

    fn toggle_pipeline(&mut self, key: &'static str, checked: bool, cx: &mut Context<Self>) {
        if self.form_disabled() {
            return;
        }
        self.set_field("pipeline", key, serde_json::Value::from(checked), cx);
    }

    fn add_entry(&mut self, group: &str, cx: &mut Context<Self>) {
        if self.form_disabled() {
            return;
        }
        let l = llm(&self.effective);
        let entries = l.get(group).cloned().unwrap_or(serde_json::Value::Null);
        let (prefix, value) = if group == "providers" {
            ("provider", serde_json::json!({"kind": "openai-compatible"}))
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
        self.open_ids.insert(format!("{group}:{name}"));
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
        self.touch(if group == "providers" {
            SettingsTab::Providers
        } else {
            SettingsTab::Models
        });
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
        self.open_ids.remove(&format!("{group}:{id}"));
        self.touch(if group == "providers" {
            SettingsTab::Providers
        } else {
            SettingsTab::Models
        });
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
        if self.open_ids.remove(&format!("{group}:{old_id}")) {
            self.open_ids.insert(format!("{group}:{new_id}"));
        }
        self.touch(if group == "providers" {
            SettingsTab::Providers
        } else {
            SettingsTab::Models
        });
        self.rederive_yaml(cx);
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

    // -- Registry field edits (provider/model details; add/delete removed) --

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
        self.touch(if group == "providers" {
            SettingsTab::Providers
        } else {
            SettingsTab::Models
        });
        self.rederive_yaml(cx);
    }

    // -- Save / validate / restore (revision-checked; 409 reloads) --

    fn do_save(&mut self, cx: &mut Context<Self>) {
        let Some(service) = self.service.clone() else {
            return;
        };
        let revision = self.saved_revision;
        let yaml_text = self.draft_yaml.clone().unwrap_or_else(|| {
            serde_json::to_string_pretty(&self.effective).unwrap_or_else(|_| "{}".into())
        });
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
                    let config = service.save_global_config_checked(&yaml_text, revision)?;
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
                        if service.save_credential(&conn, &secret).is_ok() {
                            committed.push(conn);
                        } else {
                            keys_failed = true;
                        }
                    }
                    Ok::<_, ServiceError>((config, committed, keys_failed))
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
                        view.refresh_creds(cx);
                        if keys_failed {
                            view.error = Some(
                                "Configuration saved, but an API key could not be saved. Save again to retry.".into(),
                            );
                        } else {
                            view.notice = Some("Global settings saved".into());
                        }
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

    /// Engine dry-run validation (unsaved): applies the normalized
    /// effective config on success.
    fn do_validate(&mut self, cx: &mut Context<Self>) {
        let Some(service) = self.service.clone() else {
            return;
        };
        let text = self.draft_yaml.clone().unwrap_or_else(|| {
            serde_json::to_string_pretty(&self.effective).unwrap_or_else(|_| "{}".into())
        });
        self.op = Some("validate");
        self.error = None;
        self.notice = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { service.validate_global_config(&text) })
                .await;
            _ = this.update(cx, |view: &mut GlobalSettingsView, cx| {
                view.op = None;
                match result {
                    Ok(config) => {
                        view.effective = config.effective;
                        view.sync_tier_widgets();
                        view.sync_workflow_widgets();
                        view.touched.insert(SettingsTab::Advanced);
                        view.notice = Some("Draft is valid.".into());
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

    /// Restore factory defaults (engine template, unsaved until Save).
    fn do_restore(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    Ok::<_, ServiceError>(wenyi_core::service::Service::get_global_defaults())
                })
                .await;
            _ = this.update(cx, |view: &mut GlobalSettingsView, cx| {
                match result {
                    Ok(config) => {
                        view.draft_yaml = Some(config.yaml.clone());
                        view.effective = config.effective;
                        view.sync_tier_widgets();
                        view.sync_workflow_widgets();
                        view.touched.insert(SettingsTab::Advanced);
                        view.notice = Some("Defaults restored (unsaved).".into());
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

    fn refresh_creds(&mut self, cx: &mut Context<Self>) {
        let Some(service) = self.service.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { service.credential_statuses() })
                .await;
            _ = this.update(cx, |view: &mut GlobalSettingsView, cx| {
                view.cred_statuses = result;
                cx.notify();
            });
        })
        .detach();
    }

    // -- Model name suggestions --

    fn fetch_suggestions(&mut self, model_id: &str, cx: &mut Context<Self>) {
        let Some(service) = self.service.clone() else {
            return;
        };
        let models: HashMap<String, serde_json::Value> = self.models().into_iter().collect();
        let Some(model) = models.get(model_id).cloned() else {
            return;
        };
        let conn = model
            .get("provider")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let providers: HashMap<String, serde_json::Value> = self.providers().into_iter().collect();
        let _ = providers
            .get(&conn)
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let _ = self
            .cred_drafts
            .get(&conn)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let key = model_id.to_string();
        self.fetching_models.insert(key.clone());
        self.suggestion_error.remove(&key);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { block_on(service.list_models(&conn)) })
                .await;
            _ = this.update(cx, |view: &mut GlobalSettingsView, cx| {
                view.fetching_models.remove(&key);
                match result {
                    Ok(names) => {
                        if names.is_empty() {
                            view.suggestion_error.insert(
                                key,
                                "This provider cannot list models. Enter the name.".to_string(),
                            );
                        } else {
                            view.suggestions.insert(key, names);
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
        let Some(service) = self.service.clone() else {
            return;
        };
        if !self.config_saved() || self.dirty() {
            return;
        }
        self.pinging = true;
        self.ping_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let results = cx
                .background_spawn(async move { block_on(service.ping_models(30.0)) })
                .await;
            _ = this.update(cx, |view: &mut GlobalSettingsView, cx| {
                view.pinging = false;
                view.ping_results = Some(results);
                view.ping_error = None;
                cx.notify();
            });
        })
        .detach();
    }

    // -- Widget helpers --

    fn text_input(
        &mut self,
        key: &str,
        initial: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if let Some(entity) = self.inputs.get(key) {
            return entity.clone();
        }
        let entity = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(initial, window, cx);
            state
        });
        let owned = key.to_string();
        cx.subscribe(
            &entity,
            move |this: &mut GlobalSettingsView, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.on_text_changed(&owned, cx);
                }
            },
        )
        .detach();
        self.inputs.insert(key.to_string(), entity.clone());
        entity
    }

    fn cred_input(
        &mut self,
        conn: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let key = format!("cred:{conn}");
        if let Some(entity) = self.inputs.get(&key) {
            return entity.clone();
        }
        let entity = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("API key (saved keys stay hidden)")
        });
        let conn_owned = conn.to_string();
        cx.subscribe(
            &entity,
            move |this: &mut GlobalSettingsView, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.on_cred_changed(&conn_owned, cx);
                }
            },
        )
        .detach();
        self.inputs.insert(key, entity.clone());
        entity
    }

    fn on_text_changed(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(entity) = self.inputs.get(key).cloned() else {
            return;
        };
        let value = entity.read(cx).value().to_string();
        // Workflow int fields.
        if key == "wf:segment:max_tokens_per_batch" {
            if self.form_disabled() {
                return;
            }
            if let Ok(v) = value.trim().parse::<i64>() {
                if v >= 1 {
                    self.set_field(
                        "segment",
                        "max_tokens_per_batch",
                        serde_json::Value::from(v),
                        cx,
                    );
                }
            }
            return;
        }
        if key == "wf:segment:max_tokens_per_segment" {
            if self.form_disabled() {
                return;
            }
            if let Ok(v) = value.trim().parse::<i64>() {
                if v >= 1 {
                    self.set_field(
                        "segment",
                        "max_tokens_per_segment",
                        serde_json::Value::from(v),
                        cx,
                    );
                }
            }
            return;
        }
        if key == "wf:pipeline:review_concurrency" {
            if self.form_disabled() {
                return;
            }
            if let Ok(v) = value.trim().parse::<i64>() {
                if v >= 1 {
                    self.set_field(
                        "pipeline",
                        "review_concurrency",
                        serde_json::Value::from(v),
                        cx,
                    );
                }
            }
            return;
        }
        if key == "advanced-yaml" {
            self.draft_yaml = Some(value);
            self.touch(SettingsTab::Advanced);
            cx.notify();
            return;
        }
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
            "max_input_tokens" | "max_output_tokens" => {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    serde_json::Value::Null
                } else {
                    match trimmed.parse::<i64>() {
                        Ok(v) if v >= 1 => serde_json::Value::from(v),
                        _ => return,
                    }
                }
            }
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

    fn rename_input(
        &mut self,
        group: &str,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
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
        cx.subscribe(
            &entity,
            move |this: &mut GlobalSettingsView, _, event: &InputEvent, cx| match event {
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
            },
        )
        .detach();
        self.rename_inputs.insert(key, entity.clone());
        entity
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
        if self
            .select_sigs
            .get(key)
            .map(|s| s != &sig)
            .unwrap_or(false)
        {
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
            self.set_field(
                "pipeline",
                "pdf_backend",
                serde_json::Value::from(value),
                cx,
            );
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
            self.touch(SettingsTab::Routing);
            self.set_llm(next_llm, cx);
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
            let next = if value == format!("tier:{tier_default}")
                && fallbacks.as_array().map(|a| a.is_empty()).unwrap_or(true)
            {
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
            self.touch(SettingsTab::Routing);
            self.set_llm(next_llm, cx);
            return;
        }
        if let Some(rest) = key.strip_prefix("kind:") {
            // Protocol-specific options cannot be carried to another adapter.
            let l = llm(&self.effective);
            let providers = l
                .get("providers")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
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
            self.touch(SettingsTab::Providers);
            self.set_llm(next_llm, cx);
        }
        if let Some(rest) = key.strip_prefix("conn:") {
            self.update_registry(
                "models",
                rest,
                "provider",
                serde_json::Value::from(value),
                cx,
            );
            // Reset options when the connection changes (mirrors source).
            self.update_registry("models", rest, "options", serde_json::json!({}), cx);
        }
    }

    // -- Tab renderers (one per workspace tab; bodies moved from `render`) --

    fn render_general_tab(&mut self, palette: &theme::Palette) -> Div {
        // Local prefs only (language + theme) plus the updater placeholder.
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::BOLD)
                            .child("General"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child("Interface language")
                            .child(Select::new(self.lang_select.as_ref().unwrap())),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_lg().font_weight(FontWeight::BOLD).child("Theme"))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child("Mode")
                            .child(Select::new(self.theme_mode_select.as_ref().unwrap())),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child("Accent")
                            .child(Select::new(self.theme_accent_select.as_ref().unwrap())),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(palette.border)
                    .child(div().text_sm().font_medium().child("App updates"))
                    .child(div().text_sm().text_color(palette.muted_foreground).child(
                        "The Tauri auto-updater section is not ported to the native app yet.",
                    )),
            )
    }

    fn render_providers_tab(
        &mut self,
        palette: &theme::Palette,
        form_disabled: bool,
        dirty: bool,
        config_saved: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        // One card per connection (id + kind + key/ping status), fields inline.
        // Add/delete moved out; connections are edited in place.
        let providers = self.providers();
        let mut body = div().flex().flex_col().gap_2().child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::BOLD)
                        .child(format!("API connections ({})", providers.len())),
                )
                .child(div().flex_1())
                .child(
                    Button::new("ping")
                        .label(if self.pinging {
                            "Testing…"
                        } else {
                            "Test connection"
                        })
                        .disabled(form_disabled || self.pinging || !config_saved || dirty)
                        .on_click(cx.listener(|this, _, _, cx| this.do_ping(cx))),
                )
                .child(
                    Button::new("add-provider")
                        .label("Add API connection")
                        .disabled(form_disabled)
                        .on_click(cx.listener(|this, _, _, cx| this.add_entry("providers", cx))),
                ),
        );
        if !config_saved || dirty {
            body = body.child(
                div()
                    .text_sm()
                    .text_color(palette.muted_foreground)
                    .child("Save the configuration before testing the connection."),
            );
        }
        // Last ping outcome per connection for the header status dots.
        let mut ping_ok: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
        if let Some(results) = &self.ping_results {
            for r in results {
                ping_ok.entry(r.connection.clone()).or_insert(r.ok);
            }
        }
        struct ProviderRow {
            id: String,
            kind: String,
            refs: Vec<String>,
            kind_select: Entity<SelectState<Vec<String>>>,
            url_input: Entity<InputState>,
            env_input: Entity<InputState>,
            timeout_input: Entity<InputState>,
            cred: Entity<InputState>,
            rename: Entity<InputState>,
            cred_available: bool,
            cred_mode: String,
            cred_pending: bool,
            ping: Option<bool>,
            can_delete: bool,
            open: bool,
        }
        let mut rows: Vec<ProviderRow> = Vec::with_capacity(providers.len());
        for (id, provider) in &providers {
            let kind = provider
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("openai-compatible")
                .to_string();
            let mut kind_opts = vec![kind.clone()];
            for k in &self.provider_kinds {
                if Some(k) != Some(&kind) {
                    kind_opts.push(k.clone());
                }
            }
            let refs = registry_references(&self.effective, "providers", id);
            let refs_len = refs.len();
            rows.push(ProviderRow {
                kind_select: self.ensure_select(
                    &format!("kind:{id}"),
                    kind_opts,
                    &kind,
                    window,
                    cx,
                ),
                url_input: self.text_input(
                    &format!("p:{id}:base_url"),
                    provider
                        .get("base_url")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    window,
                    cx,
                ),
                env_input: self.text_input(
                    &format!("p:{id}:api_key_env"),
                    provider
                        .get("api_key_env")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    window,
                    cx,
                ),
                timeout_input: self.text_input(
                    &format!("p:{id}:timeout"),
                    provider
                        .get("timeout")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(600)
                        .to_string(),
                    window,
                    cx,
                ),
                cred: self.cred_input(id, window, cx),
                rename: self.rename_input("providers", id, window, cx),
                cred_available: self
                    .cred_statuses
                    .iter()
                    .find(|s| s.connection == *id)
                    .map(|s| s.configured)
                    .unwrap_or(false),
                cred_mode: self
                    .cred_statuses
                    .iter()
                    .find(|s| s.connection == *id)
                    .map(|s| s.source.clone())
                    .unwrap_or_else(|| "environment".to_string()),
                cred_pending: self
                    .cred_drafts
                    .get(id)
                    .map(|v| !v.trim().is_empty())
                    .unwrap_or(false),
                ping: ping_ok.get(id).copied(),
                can_delete: refs_len == 0,
                open: self.open_ids.contains(&format!("providers:{id}")),
                id: id.clone(),
                refs,
                kind,
            });
        }
        for row in rows {
            // Header status dot: ping result wins, else key state.
            let (dot_color, dot_label) = match (row.ping, row.cred_available, row.cred_pending) {
                (Some(true), _, _) => (palette.success, "reachable"),
                (Some(false), _, _) => (palette.destructive, "unreachable"),
                (None, _, true) => (palette.warning, "unsaved key"),
                (None, true, _) => (palette.success, "key saved"),
                (None, false, _) => (palette.muted_foreground, "no key"),
            };
            let key_label = format!(
                "API key{}",
                if row.cred_available {
                    format!(" (saved via {})", row.cred_mode)
                } else {
                    " (not set)".to_string()
                }
            );
            let key_hint: &str = if row.cred_pending {
                "Unsaved key — it is written after the configuration saves."
            } else {
                "Saved keys stay hidden; typing a key stages it for the next save."
            };
            let used_by = if row.refs.is_empty() {
                "unused".to_string()
            } else {
                format!(
                    "{} model{}",
                    row.refs.len(),
                    if row.refs.len() == 1 { "" } else { "s" }
                )
            };
            let open = row.open;
            let tog = row.id.clone();
            let del_id = row.id.clone();
            let editing = self.editing_ids.contains(&format!("providers:{}", row.id));
            let header = div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .min_w_0()
                .child(
                    div()
                        .w(px(8.0))
                        .h(px(8.0))
                        .rounded_full()
                        .bg(dot_color)
                        .flex_none(),
                )
                .child(
                    div()
                        .truncate()
                        .flex_1()
                        .font_weight(FontWeight::MEDIUM)
                        .child(row.id.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .px_1p5()
                        .py_0p5()
                        .rounded_sm()
                        .bg(palette.muted)
                        .text_color(palette.muted_foreground)
                        .flex_none()
                        .child(row.kind.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(palette.muted_foreground)
                        .flex_none()
                        .child(dot_label.to_string()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(palette.muted_foreground)
                        .flex_none()
                        .child(used_by),
                )
                .child(
                    Button::new(format!("tog-provider-{}", row.id))
                        .label(if open { "▾" } else { "▸" })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let k = format!("providers:{tog}");
                            if !this.open_ids.remove(&k) {
                                this.open_ids.insert(k);
                            }
                            cx.notify();
                        })),
                );
            let mut entry = div()
                .flex()
                .flex_col()
                .rounded_md()
                .border_1()
                .border_color(palette.border)
                .child(header);
            if open {
                entry = entry.child(
                    div().flex().flex_col().gap_2().px_3().pb_3()
                        .child(
                            div().flex().flex_col().gap_1()
                                .child("Connection ID (rename; Enter/blur applies)")
                                .child(Input::new(&row.rename).disabled(form_disabled)),
                        )
                        .child(
                            div().flex().gap_2()
                                .child(
                                    div().flex_1().flex().flex_col().gap_1().child("API provider").child(
                                        Select::new(&row.kind_select).disabled(form_disabled),
                                    ),
                                )
                                .child(
                                    div().flex_1().flex().flex_col().gap_1()
                                        .child("Request timeout (seconds)")
                                        .child(Input::new(&row.timeout_input).disabled(form_disabled)),
                                ),
                        )
                        .child(
                            div().flex().flex_col().gap_1()
                                .child("API base URL (leave blank for the provider default URL)")
                                .child(Input::new(&row.url_input).disabled(form_disabled)),
                        )
                        .child(
                            div().flex().flex_col().gap_1()
                                .child("API key environment variable (leave blank for the provider default variable)")
                                .child(Input::new(&row.env_input).disabled(form_disabled)),
                        )
                        .child(
                            div().flex().flex_col().gap_1()
                                .child(key_label)
                                .child(Input::new(&row.cred).disabled(form_disabled))
                                .child(div().text_sm().text_color(palette.muted_foreground).child(key_hint)),
                        )
                        .child(
                            div().flex().gap_2().items_center()
                                .child(
                                    Button::new(format!("del-provider-{}", row.id))
                                        .label(if editing { "Apply ID edit first" } else { "Delete" })
                                        .disabled(form_disabled || !row.can_delete || editing)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.remove_entry("providers", &del_id, cx)
                                        })),
                                )
                                .child(
                                    div().text_sm().text_color(palette.muted_foreground).child(
                                        if row.can_delete {
                                            "Unused connections can be deleted.".to_string()
                                        } else {
                                            format!("In use by: {}", row.refs.join(", "))
                                        },
                                    ),
                                ),
                        ),
                );
            }
            body = body.child(entry);
        }
        // Connectivity results (the Test button lives in the tab header;
        // mirrors ModelConnectivity, saved-only gate).
        if let Some(err) = &self.ping_error {
            body = body.child(
                div()
                    .p_3()
                    .rounded_md()
                    .bg(palette.destructive)
                    .text_color(palette.destructive_foreground)
                    .child(format!("Error: {err}")),
            );
        }
        if let Some(results) = &self.ping_results {
            let mut cbody = div().flex().flex_col().gap_2();
            if results.is_empty() {
                cbody = cbody.child(div().text_sm().child("No models are configured."));
            } else {
                let failed = results.iter().filter(|r| !r.ok).count();
                cbody = cbody.child(div().text_sm().child(if failed == 0 {
                    format!("All {} models are reachable.", results.len())
                } else {
                    format!("{failed} of {} models are unreachable.", results.len())
                }));
                for r in results {
                    let detail = r
                        .error
                        .clone()
                        .unwrap_or_else(|| "Probe failed.".to_string());
                    let models = if r.models.is_empty() {
                        "—".to_string()
                    } else {
                        r.models.join(", ")
                    };
                    cbody = cbody.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .p_2()
                            .rounded_md()
                            .border_1()
                            .border_color(palette.border)
                            .child(
                                div()
                                    .text_sm()
                                    .child(format!("{} · {models}", r.connection)),
                            )
                            .child(div().text_sm().child(format!(
                                "{} · {detail}",
                                if r.ok { "Connected" } else { "Failed" }
                            ))),
                    );
                }
            }
            body = body.child(cbody);
        }
        body
    }

    fn render_models_tab(
        &mut self,
        palette: &theme::Palette,
        form_disabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        // One card per model: connection select + model name + suggestions.
        // Add/delete moved out; models are edited in place.
        let models = self.models();
        let provider_ids: Vec<String> = self.providers().into_iter().map(|(id, _)| id).collect();
        let mut body = div().flex().flex_col().gap_2().child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_lg()
                        .font_weight(FontWeight::BOLD)
                        .child(format!("Models ({})", models.len())),
                )
                .child(div().flex_1())
                .child(
                    Button::new("add-model")
                        .label("Add model")
                        .disabled(form_disabled)
                        .on_click(cx.listener(|this, _, _, cx| this.add_entry("models", cx))),
                ),
        );
        struct ModelRow {
            id: String,
            conn: String,
            refs: Vec<String>,
            conn_select: Entity<SelectState<Vec<String>>>,
            name_input: Entity<InputState>,
            max_input: Entity<InputState>,
            max_output: Entity<InputState>,
            rename: Entity<InputState>,
            fetching: bool,
            suggestions: Option<Vec<String>>,
            suggestion_error: Option<String>,
            can_delete: bool,
            open: bool,
        }
        let mut rows: Vec<ModelRow> = Vec::with_capacity(models.len());
        for (id, model) in &models {
            let conn = model
                .get("provider")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let name = model
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let max_in = model
                .get("max_input_tokens")
                .and_then(|v| v.as_i64())
                .map(|v| v.to_string())
                .unwrap_or_default();
            let max_out = model
                .get("max_output_tokens")
                .and_then(|v| v.as_i64())
                .map(|v| v.to_string())
                .unwrap_or_default();
            let model_refs = registry_references(&self.effective, "models", id);
            rows.push(ModelRow {
                conn_select: self.ensure_select(
                    &format!("conn:{id}"),
                    provider_ids.clone(),
                    &conn,
                    window,
                    cx,
                ),
                name_input: self.text_input(&format!("m:{id}:name"), name, window, cx),
                max_input: self.text_input(&format!("m:{id}:max_input_tokens"), max_in, window, cx),
                max_output: self.text_input(
                    &format!("m:{id}:max_output_tokens"),
                    max_out,
                    window,
                    cx,
                ),
                rename: self.rename_input("models", id, window, cx),
                fetching: self.fetching_models.contains(id),
                suggestions: self.suggestions.get(id).cloned(),
                suggestion_error: self.suggestion_error.get(id).cloned(),
                can_delete: model_refs.is_empty(),
                open: self.open_ids.contains(&format!("models:{id}")),
                id: id.clone(),
                refs: model_refs,
                conn,
            });
        }
        for row in rows {
            let id_fetch = row.id.clone();
            let tog = row.id.clone();
            let del_id = row.id.clone();
            let open = row.open;
            let editing = self.editing_ids.contains(&format!("models:{}", row.id));
            let mut fields = div()
                .flex()
                .flex_col()
                .gap_2()
                .px_3()
                .pb_3()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child("Model ID (rename; Enter/blur applies)")
                        .child(Input::new(&row.rename).disabled(form_disabled)),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child("API connection")
                                .child(Select::new(&row.conn_select).disabled(form_disabled)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child("Model name")
                                .child(Input::new(&row.name_input).disabled(form_disabled)),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child("Max input tokens (empty = no limit)")
                                .child(Input::new(&row.max_input).disabled(form_disabled)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child("Max output tokens (empty = operation default)")
                                .child(Input::new(&row.max_output).disabled(form_disabled)),
                        ),
                );
            // Suggestions (mirrors ModelNameField fetch).
            if row.fetching {
                fields = fields.child(
                    div()
                        .text_sm()
                        .text_color(palette.muted_foreground)
                        .child("Loading model names…"),
                );
            } else if let Some(names) = &row.suggestions {
                if names.is_empty() {
                    fields = fields.child(
                        div()
                            .text_sm()
                            .text_color(palette.muted_foreground)
                            .child("No models returned. Enter the name."),
                    );
                } else {
                    let mut list = div().flex().flex_col().gap_1();
                    for n in names.iter().take(20) {
                        list = list.child(div().text_sm().child(n.clone()));
                    }
                    fields = fields.child(list);
                }
            } else if let Some(err) = &row.suggestion_error {
                fields = fields.child(
                    div()
                        .text_sm()
                        .text_color(palette.muted_foreground)
                        .child(err.clone()),
                );
            }
            fields = fields.child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new(format!("fetch-models-{}", row.id))
                            .label("Fetch model names")
                            .disabled(form_disabled)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.fetch_suggestions(&id_fetch, cx)
                            })),
                    )
                    .child(
                        Button::new(format!("del-model-{}", row.id))
                            .label(if editing {
                                "Apply ID edit first"
                            } else {
                                "Delete"
                            })
                            .disabled(form_disabled || !row.can_delete || editing)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_entry("models", &del_id, cx)
                            })),
                    ),
            );
            if !row.can_delete {
                fields = fields.child(
                    div()
                        .text_sm()
                        .text_color(palette.muted_foreground)
                        .child(format!("In use by: {}", row.refs.join(", "))),
                );
            }
            let mut entry = div()
                .flex()
                .flex_col()
                .rounded_md()
                .border_1()
                .border_color(palette.border)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .min_w_0()
                        .child(
                            div()
                                .truncate()
                                .flex_1()
                                .font_weight(FontWeight::MEDIUM)
                                .child(row.id.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .px_1p5()
                                .py_0p5()
                                .rounded_sm()
                                .bg(palette.muted)
                                .text_color(palette.muted_foreground)
                                .flex_none()
                                .child(if row.conn.is_empty() {
                                    "—".to_string()
                                } else {
                                    row.conn.clone()
                                }),
                        )
                        .child(
                            Button::new(format!("tog-model-{}", row.id))
                                .label(if open { "▾" } else { "▸" })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let k = format!("models:{tog}");
                                    if !this.open_ids.remove(&k) {
                                        this.open_ids.insert(k);
                                    }
                                    cx.notify();
                                })),
                        ),
                );
            if open {
                entry = entry.child(fields);
            }
            body = body.child(entry);
        }
        body
    }

    fn render_routing_tab(
        &mut self,
        palette: &theme::Palette,
        form_disabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let model_ids: Vec<String> = self.models().into_iter().map(|(id, _)| id).collect();
        let mut body = div().flex().flex_col().gap_2().child(
            div()
                .text_lg()
                .font_weight(FontWeight::BOLD)
                .child("Routing (tiers + per-operation)"),
        );
        if self.operations.is_empty() {
            body = body.child(
                div().text_sm().text_color(palette.muted_foreground).child(
                    "Engine `Capabilities.operations` is not available yet (requested). Tier defaults below still work.",
                ),
            );
        }
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
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(*label)
                    .child(Select::new(&select).disabled(form_disabled)),
            );
        }
        body = body.child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_medium()
                        .child("Default models per tier"),
                )
                .child(tiers),
        );
        if self.operations.is_empty() {
            return body;
        }
        let mut grouped: Vec<(String, Vec<serde_json::Value>)> = Vec::new();
        for op in self.operations.clone() {
            let oid = op
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if oid.is_empty() {
                continue;
            }
            let tier = op
                .get("tier")
                .and_then(|v| v.as_str())
                .unwrap_or("strong")
                .to_string();
            match grouped.iter_mut().find(|(t, _)| *t == tier) {
                Some((_, ops)) => ops.push(op),
                None => grouped.push((tier, vec![op])),
            }
        }
        grouped.sort_by_key(|(t, _)| {
            TIERS
                .iter()
                .position(|(id, _)| id == t)
                .unwrap_or(usize::MAX)
        });
        let tier_label = |t: &str| -> String {
            TIERS
                .iter()
                .find(|(id, _)| *id == t)
                .map(|(_, l)| l.to_string())
                .unwrap_or_else(|| t.to_string())
        };
        let mut ops = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_sm().font_medium().child("Models by operation"));
        for (tier, tier_ops) in &grouped {
            let mut group = div().flex().flex_col().gap_1().child(
                div()
                    .text_xs()
                    .font_medium()
                    .text_color(palette.muted_foreground)
                    .child(format!("{} — default {}", tier_label(tier), tier_ops.len(),)),
            );
            for op in tier_ops {
                let oid = op
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let desc = op
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&oid)
                    .to_string();
                let current = self
                    .op_values
                    .get(&oid)
                    .cloned()
                    .unwrap_or_else(|| format!("tier:{tier}"));
                let mut opts: Vec<String> =
                    TIERS.iter().map(|(t, _)| format!("tier:{t}")).collect();
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
                    .map(|a| {
                        a.iter()
                            .filter_map(|f| f.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let overridden = !current.starts_with("tier:") || !fallbacks.is_empty();
                let mut row = div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_1p5()
                    .rounded_md()
                    .border_1()
                    .border_color(palette.border)
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(desc));
                if overridden {
                    row = row.child(
                        div()
                            .text_xs()
                            .px_1p5()
                            .py_0p5()
                            .rounded_sm()
                            .bg(palette.muted)
                            .text_color(palette.muted_foreground)
                            .flex_none()
                            .child(if fallbacks.is_empty() {
                                "override".to_string()
                            } else {
                                format!(
                                    "+{} fallback{}",
                                    fallbacks.len(),
                                    if fallbacks.len() == 1 { "" } else { "s" }
                                )
                            }),
                    );
                }
                row = row.child(
                    div()
                        .w(px(220.0))
                        .flex_none()
                        .child(Select::new(&select).disabled(form_disabled)),
                );
                group = group.child(row);
            }
            ops = ops.child(group);
        }
        body = body.child(ops);
        body
    }

    fn render_workflow_tab(
        &mut self,
        palette: &theme::Palette,
        form_disabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let pipeline_book = get_bool(&self.effective, "pipeline", "book_understanding");
        let pipeline_polish = get_bool(&self.effective, "pipeline", "polish");
        let pipeline_review = get_bool(&self.effective, "pipeline", "review");
        let pipeline_autofix = get_bool(&self.effective, "pipeline", "review_autofix");
        let annotation = get_bool(&self.effective, "pipeline", "annotation_alignment");
        let pdf_current = self.pdf_backend.clone();
        let pdf_select = self.ensure_select(
            "pdf_backend",
            vec!["mineru".to_string(), "babeldoc".to_string()],
            &pdf_current,
            window,
            cx,
        );
        let seg_batch = self.text_input(
            "wf:segment:max_tokens_per_batch",
            get_i64(&self.effective, "segment", "max_tokens_per_batch", 1800).to_string(),
            window,
            cx,
        );
        let seg_para = self.text_input(
            "wf:segment:max_tokens_per_segment",
            get_i64(&self.effective, "segment", "max_tokens_per_segment", 1200).to_string(),
            window,
            cx,
        );
        let rev_conc = self.text_input(
            "wf:pipeline:review_concurrency",
            get_i64(&self.effective, "pipeline", "review_concurrency", 4).to_string(),
            window,
            cx,
        );
        div().flex().flex_col().gap_3()
            .child(
                div().flex().flex_col().gap_2()
                    .child(div().text_lg().font_weight(FontWeight::BOLD).child("Workflow"))
                    .child(Checkbox::new("wf-book").label("Book understanding").checked(pipeline_book).on_change(cx.listener(|this, checked: &bool, _, cx| this.toggle_pipeline("book_understanding", *checked, cx))))
                    .child(Checkbox::new("wf-polish").label("Polishing").checked(pipeline_polish).on_change(cx.listener(|this, checked: &bool, _, cx| this.toggle_pipeline("polish", *checked, cx))))
                    .child(Checkbox::new("wf-review").label("Whole-book review").checked(pipeline_review).on_change(cx.listener(|this, checked: &bool, _, cx| this.toggle_pipeline("review", *checked, cx))))
                    .child(Checkbox::new("wf-autofix").label("Apply autofixes after review").checked(pipeline_autofix).on_change(cx.listener(|this, checked: &bool, _, cx| this.toggle_pipeline("review_autofix", *checked, cx))))
                    .child(div().text_sm().font_medium().child("Segmentation and performance"))
                    .child(Checkbox::new("wf-align").label("Paragraph annotation alignment").checked(annotation).on_change(cx.listener(|this, checked: &bool, _, cx| this.toggle_pipeline("annotation_alignment", *checked, cx))))
                    .child(
                        div().flex().gap_2()
                            .child(div().flex_1().flex().flex_col().gap_1().child("Tokens per batch").child(Input::new(&seg_batch).disabled(form_disabled)))
                            .child(div().flex_1().flex().flex_col().gap_1().child("Tokens per paragraph").child(Input::new(&seg_para).disabled(form_disabled))),
                    )
                    .child(
                        div().flex().gap_2()
                            .child(div().flex_1().flex().flex_col().gap_1().child("Review concurrency").child(Input::new(&rev_conc).disabled(form_disabled)))
                            .child(div().flex_1().flex().flex_col().gap_1().child("PDF parser").child(Select::new(&pdf_select).disabled(form_disabled))),
                    ),
            )
            .child(
                div().flex().flex_col().gap_1().p_3().rounded_md().border_1().border_color(palette.border)
                    .child(div().text_sm().font_medium().child("MinerU PDF parsing"))
                    .child(div().text_sm().text_color(palette.muted_foreground).child(
                        "MinerU key management is not available yet (engine `mineru_status`/`save_mineru` pending).",
                    )),
            )
    }

    fn render_advanced_tab(
        &mut self,
        palette: &theme::Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let draft = self.draft_yaml.clone().unwrap_or_default();
        let yaml_input = if let Some(e) = self.yaml_input.clone() {
            e
        } else {
            let e = cx.new(|cx| {
                let mut state = InputState::new(window, cx);
                state.set_value(draft.clone(), window, cx);
                state
            });
            let owned = "advanced-yaml".to_string();
            cx.subscribe(
                &e,
                move |this: &mut GlobalSettingsView, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        if let Some(ent) = this.inputs.get(&owned).cloned() {
                            this.draft_yaml = Some(ent.read(cx).value().to_string());
                            this.touched.insert(SettingsTab::Advanced);
                            cx.notify();
                        }
                    }
                },
            )
            .detach();
            self.inputs.insert("advanced-yaml".to_string(), e.clone());
            self.yaml_input = Some(e.clone());
            e
        };
        if yaml_input.read(cx).value().as_ref() != draft {
            yaml_input.update(cx, |state, cx| {
                state.set_value(draft.clone(), window, cx);
            });
        }
        let dirty_adv = self.draft_yaml.as_deref().unwrap_or("")
            != self.saved_yaml.as_deref().unwrap_or("\u{0}");
        let mut body = div().flex().flex_col().flex_1().min_h_0().gap_2()
            .child(div().text_lg().font_weight(FontWeight::BOLD).child("Advanced"))
            .child(
                div().flex().flex_col().flex_1().min_h_0().gap_1()
                    .child("Configuration draft (JSON; YAML validation pending engine support)")
                    .child(Input::new(&yaml_input).disabled(!self.loaded || self.op.is_some())),
            )
            .child(
                div().text_xs().text_color(palette.muted_foreground).child(
                    "Server-side Validate/RestoreRevision are not available yet (engine pending); Save writes last-write-wins.",
                ),
            );
        if dirty_adv {
            body = body.child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .text_sm()
                            .text_color(palette.muted_foreground)
                            .flex_1()
                            .child("Draft differs from the saved configuration. Save to apply."),
                    )
                    .child(
                        Button::new("validate-yaml")
                            .label("Validate")
                            .disabled(
                                !self.loaded || self.op.is_some() || !self.editing_ids.is_empty(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.do_validate(cx))),
                    ),
            );
        }
        body
    }

    fn render_action_bar(
        &mut self,
        palette: &theme::Palette,
        op_busy: bool,
        dirty: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let save_label = if op_busy {
            "Saving…"
        } else {
            "Save configuration"
        };
        let editing = !self.editing_ids.is_empty();
        div()
            .flex()
            .gap_2()
            .items_center()
            .child(
                Button::new("restore")
                    .ghost()
                    .label("Restore defaults")
                    .disabled(!self.loaded || op_busy)
                    .on_click(cx.listener(|this, _, _, cx| this.do_restore(cx))),
            )
            .child(
                Button::new("validate")
                    .label("Validate configuration")
                    .disabled(!self.loaded || op_busy || editing)
                    .on_click(cx.listener(|this, _, _, cx| this.do_validate(cx))),
            )
            .child(
                Button::new("save")
                    .label(save_label)
                    .primary()
                    .disabled(!self.loaded || op_busy || editing)
                    .on_click(cx.listener(|this, _, _, cx| this.do_save(cx))),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(palette.muted_foreground)
                    .child(if editing {
                        "Apply ID edits before saving."
                    } else if dirty {
                        "Unsaved changes"
                    } else {
                        ""
                    }),
            )
    }
}

impl Render for GlobalSettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current_palette();

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
            let opts = vec![
                "system".to_string(),
                "dark".to_string(),
                "light".to_string(),
                "noctalia".to_string(),
            ];
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
                        let _ = h.update(cx, |app, cx| {
                            app.theme_choice = choice;
                            cx.notify();
                        });
                    }
                    cx.notify();
                }
            })
            .detach();
            self.theme_mode_select = Some(entity);
        }
        if self.theme_accent_select.is_none() {
            let opts: Vec<String> = theme::Accent::all()
                .iter()
                .map(|a| a.name().to_string())
                .collect();
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
                        let _ = h.update(cx, |app, cx| {
                            app.theme_choice = choice;
                            cx.notify();
                        });
                    }
                    cx.notify();
                }
            })
            .detach();
            self.theme_accent_select = Some(entity);
        }
        let form_disabled = self.form_disabled();
        let op_busy = self.op.is_some();
        let dirty = self.dirty();
        let config_saved = self.config_saved();

        // Shell: pinned header (title + banners + tabs), scrolling tab body,
        // pinned footer (action bar). Only the active tab builds elements.
        let mut col = div()
            .flex()
            .flex_col()
            .h_full()
            .min_h_0()
            .font(theme::fonts::ui())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .px_6()
                    .pt_6()
                    .child(super::ui::page_title("Settings", None::<Div>)),
            );

        if let Some(err) = &self.error {
            col = col.child(div().px_6().child(super::ui::error_box(err, &palette)));
        }
        if let Some(notice) = &self.notice {
            col = col.child(
                div().px_6().child(
                    div()
                        .p_3()
                        .rounded_md()
                        .border_1()
                        .border_color(palette.border)
                        .child(notice.clone()),
                ),
            );
        }
        if self.loading && !self.loaded {
            col = col.child(
                div().px_6().pb_6().child(
                    div()
                        .text_sm()
                        .text_color(palette.muted_foreground)
                        .child("Loading…"),
                ),
            );
            return col;
        }

        // Workspace tabs: General default; switching is render-only, no backend fetch.
        let tab_index = SettingsTab::ALL
            .iter()
            .position(|(t, _)| *t == self.tab)
            .unwrap_or(0);
        let mut tabbar = TabBar::new("settings-tabs")
            .underline()
            .selected_index(tab_index)
            .on_click(cx.listener(|this, index: &usize, _, cx| {
                if let Some((tab, _)) = SettingsTab::ALL.get(*index) {
                    this.tab = *tab;
                    cx.notify();
                }
            }));
        for (tab, label) in SettingsTab::ALL {
            let mut t = Tab::new();
            if self.tab_dirty(*tab) {
                t = t.label(format!("• {label}")).text_color(palette.primary);
            } else {
                t = t.label(*label);
            }
            tabbar = tabbar.child(t);
        }
        col = col.child(div().px_6().child(tabbar));

        // Scrolling body: only the active tab builds elements.
        let tab_body = match self.tab {
            SettingsTab::General => self.render_general_tab(&palette),
            SettingsTab::Providers => {
                self.render_providers_tab(&palette, form_disabled, dirty, config_saved, window, cx)
            }
            SettingsTab::Models => self.render_models_tab(&palette, form_disabled, window, cx),
            SettingsTab::Routing => self.render_routing_tab(&palette, form_disabled, window, cx),
            SettingsTab::Workflow => self.render_workflow_tab(&palette, form_disabled, window, cx),
            SettingsTab::Advanced => self.render_advanced_tab(&palette, window, cx),
        };
        col = col.child(
            div()
                .id("settings-tab-scroll")
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_4()
                        .p_6()
                        .w_full()
                        .max_w(px(1040.0))
                        .child(tab_body),
                ),
        );

        // Pinned footer: Save always visible.
        col = col.child(
            div()
                .px_6()
                .py_3()
                .border_t_1()
                .border_color(palette.border)
                .child(self.render_action_bar(&palette, op_busy, dirty, cx)),
        );

        col
    }
}
