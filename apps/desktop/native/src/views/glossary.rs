//! Glossary (screen 4): terms table + add/edit/delete + conflicts.
//!
//! Parity source: `features/glossary/GlossaryPage.tsx`.
//! - Search `q` + type filter (server-side params, no sorting in source).
//! - Terms list with checkbox select-all, per-row edit/delete, batch delete
//!   with confirm. Busy (`isProjectBusy`) disables mutations.
//! - Conflict queue: keep current / accept suggestion.
//! - Add/Edit dialogs: real Input entities for source/target/reading/
//!   gender/aliases/note + type Select; comma-separated aliases mirror the
//!   React `split(",")` mapping on update.
//! - Import: rfd pick → client-side CSV/JSON parse → overwrite/skip conflict
//!   step → `import_glossary`. Export: format pick → `export_glossary` bytes
//!   → rfd save dialog → write file.
//! Term types mirror `termTypes()` in the source.

use std::collections::HashSet;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Disableable, IndexPath, StyledExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::{
    client::{block_on, is_project_busy, Client, GlossaryTerm, TermConflict, TermIn},
    theme,
};

/// Term types — mirrors `termTypes()` in `GlossaryPage.tsx`.
const TERM_TYPES: &[(&str, &str)] = &[
    ("person", "Person"),
    ("term", "Terms"),
    ("appellation", "Appellation"),
    ("honorific", "Honorific"),
    ("speech", "Speech habit"),
    ("fixed_expression", "Fixed expression"),
];

fn term_type_label(t: &str) -> &str {
    TERM_TYPES.iter().find(|(id, _)| *id == t).map(|(_, l)| *l).unwrap_or(t)
}

pub struct GlossaryView {
    client: Option<Client>,
    pid: String,
    project_status: String,
    terms: Vec<GlossaryTerm>,
    /// Cached filtered view; recomputed only on terms/search/type_filter change.
    filtered: Vec<GlossaryTerm>,
    conflicts: Vec<TermConflict>,
    loading: bool,
    error: Option<String>,
    search: Entity<InputState>,
    search_text: String,
    type_filter: String,
    selected: HashSet<String>,
    /// Add dialog open; edit target when Some(source).
    editing: Option<Option<String>>,
    form: TermIn,
    form_error: Option<String>,
    saving: bool,
    /// Real form inputs (entities persist while the dialog is open; values
    /// sync out of `form` on open and back into `form` on save).
    form_source: Option<Entity<InputState>>,
    form_target: Option<Entity<InputState>>,
    form_reading: Option<Entity<InputState>>,
    form_gender: Option<Entity<InputState>>,
    form_aliases: Option<Entity<InputState>>,
    form_note: Option<Entity<InputState>>,
    form_type_select: Option<Entity<SelectState<Vec<String>>>>,
    form_type_options: Vec<String>,
    /// Import flow state (mirrors the React ImportDialog steps).
    import_open: bool,
    import_conflicts: Vec<ImportConflict>,
    import_parsed: Vec<TermIn>,
    import_error: Option<String>,
    importing: bool,
    /// Export menu / in-flight flag.
    export_open: bool,
    exporting: bool,
}

/// Import-time conflict for an incoming term whose source already exists.
/// Mirrors `ImportConflict` in `GlossaryPage.tsx` (`skip`/`overwrite`).
#[derive(Debug, Clone)]
struct ImportConflict {
    source: String,
    existing_target: String,
    new_target: String,
    decision: ImportDecision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImportDecision {
    Skip,
    Overwrite,
}

impl GlossaryView {
    pub fn new(pid: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search terms…"));
        Self {
            client: None,
            pid,
            project_status: String::new(),
            terms: Vec::new(),
            filtered: Vec::new(),
            conflicts: Vec::new(),
            loading: true,
            error: None,
            search,
            search_text: String::new(),
            type_filter: String::new(),
            selected: HashSet::new(),
            editing: None,
            form: TermIn::default(),
            form_error: None,
            saving: false,
            form_source: None,
            form_target: None,
            form_reading: None,
            form_gender: None,
            form_aliases: None,
            form_note: None,
            form_type_select: None,
            form_type_options: Vec::new(),
            import_open: false,
            import_conflicts: Vec::new(),
            import_parsed: Vec::new(),
            import_error: None,
            importing: false,
            export_open: false,
            exporting: false,
        }
    }

    pub fn set_client(&mut self, client: Client, cx: &mut Context<Self>) {
        self.client = Some(client);
        self.refresh(cx);
    }

    fn busy(&self) -> bool {
        is_project_busy(&self.project_status)
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let (Some(client), pid) = (self.client.clone(), self.pid.clone()) else {
            return;
        };
        // Read search text from the input entity (debounce omitted in U-milestone;
        // refresh triggers on explicit search submit + after mutations).
        self.search_text = self.search.read(cx).value().to_string();
        let (q, type_filter) = (self.search_text.clone(), self.type_filter.clone());
        self.loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let terms = block_on(client.list_terms(
                        &pid,
                        (!q.is_empty()).then_some(q.as_str()),
                        (!type_filter.is_empty()).then_some(type_filter.as_str()),
                    ))?;
                    let conflicts = block_on(client.list_conflicts(&pid))?;
                    let project = block_on(client.get_project(&pid))?;
                    Ok::<_, crate::client::Error>((terms, conflicts, project.project.status))
                })
                .await;
            _ = this.update(cx, |view: &mut GlossaryView, cx| {
                view.loading = false;
                match result {
                    Ok((terms, conflicts, status)) => {
                        view.terms = terms;
                        // Cached filtered view; recompute only here, not per-frame.
                        view.filtered = view.terms.clone();
                        view.conflicts = conflicts;
                        view.project_status = status;
                        view.error = None;
                        // Drop selections for vanished terms.
                        view.selected.retain(|s| view.terms.iter().any(|t| &t.source == s));
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

    fn open_add(&mut self, cx: &mut Context<Self>) {
        self.form = TermIn::default();
        self.form_error = None;
        self.editing = Some(None);
        // Fresh entities so stale edit values never leak into Add.
        self.form_source = None;
        self.form_target = None;
        self.form_reading = None;
        self.form_gender = None;
        self.form_aliases = None;
        self.form_note = None;
        self.form_type_select = None;
        self.form_type_options = Vec::new();
        cx.notify();
    }

    fn open_edit(&mut self, term: &GlossaryTerm, cx: &mut Context<Self>) {
        self.form = TermIn {
            source: term.source.clone(),
            target: term.target.clone(),
            reading: term.reading.clone(),
            r#type: if term.r#type.is_empty() { "term".into() } else { term.r#type.clone() },
            gender: term.gender.clone(),
            aliases: term.aliases.clone(),
            note: term.note.clone(),
        };
        self.form_error = None;
        self.editing = Some(Some(term.source.clone()));
        // Fresh entities so values sync out of `form` on next render.
        self.form_source = None;
        self.form_target = None;
        self.form_reading = None;
        self.form_gender = None;
        self.form_aliases = None;
        self.form_note = None;
        self.form_type_select = None;
        self.form_type_options = Vec::new();
        cx.notify();
    }

    fn close_form(&mut self, cx: &mut Context<Self>) {
        self.editing = None;
        self.form_source = None;
        self.form_target = None;
        self.form_reading = None;
        self.form_gender = None;
        self.form_aliases = None;
        self.form_note = None;
        self.form_type_select = None;
        self.form_type_options = Vec::new();
        cx.notify();
    }

    /// Pull widget values into `form` (aliases: comma-separated, mirrors the
    /// React Edit dialog's `split(",")` mapping).
    fn sync_form_from_widgets(&mut self, cx: &App) {
        if let Some(e) = &self.form_source {
            self.form.source = e.read(cx).value().to_string();
        }
        if let Some(e) = &self.form_target {
            self.form.target = e.read(cx).value().to_string();
        }
        if let Some(e) = &self.form_reading {
            self.form.reading = e.read(cx).value().to_string();
        }
        if let Some(e) = &self.form_gender {
            self.form.gender = e.read(cx).value().to_string();
        }
        if let Some(e) = &self.form_aliases {
            self.form.aliases = e
                .read(cx)
                .value()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        if let Some(e) = &self.form_note {
            self.form.note = e.read(cx).value().to_string();
        }
    }

    fn save_form(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.sync_form_from_widgets(cx);
        if self.form.source.trim().is_empty() || self.form.target.trim().is_empty() {
            self.form_error = Some("Source and target are required".into());
            cx.notify();
            return;
        }
        let pid = self.pid.clone();
        let form = self.form.clone();
        let edit_key = self.editing.clone().flatten();
        self.saving = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    match edit_key {
                        Some(source) => block_on(client.update_term(&pid, &source, &form))
                            .map(|_| ()),
                        None => block_on(client.add_term(&pid, &form)).map(|_| ()),
                    }
                })
                .await;
            _ = this.update(cx, |view: &mut GlossaryView, cx| {
                view.saving = false;
                match result {
                    Ok(()) => {
                        view.editing = None;
                        view.form_source = None;
                        view.form_target = None;
                        view.form_reading = None;
                        view.form_gender = None;
                        view.form_aliases = None;
                        view.form_note = None;
                        view.form_type_select = None;
                        view.form_type_options = Vec::new();
                        view.refresh(cx);
                    }
                    Err(err) => {
                        view.form_error = Some(err.to_string());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn delete_one(&mut self, source: String, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let pid = self.pid.clone();
        cx.spawn(async move |this, cx| {
            let result =
                cx.background_spawn(async move { block_on(client.delete_term(&pid, &source)) })
                    .await;
            _ = this.update(cx, |view: &mut GlossaryView, cx| {
                match result {
                    Ok(_) => view.refresh(cx),
                    Err(err) => {
                        view.error = Some(err.to_string());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            return;
        }
        let count = self.selected.len();
        let this = cx.entity();
        super::confirm_destructive(
            window,
            cx,
            "Delete selected terms",
            format!("Delete {count} terms? This cannot be undone."),
            "Delete",
            move |_window, cx| {
                this.update(cx, |view: &mut GlossaryView, cx| {
                    view.delete_selected_confirmed(cx);
                });
                true
            },
        );
    }

    /// Run the confirmed batch delete (same backend loop as before; ErrorNotice
    /// stays the primary error surface).
    fn delete_selected_confirmed(&mut self, cx: &mut Context<Self>) {
        let (Some(client), pid) = (self.client.clone(), self.pid.clone()) else {
            return;
        };
        let sources: Vec<String> = self.selected.iter().cloned().collect();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    for source in &sources {
                        block_on(client.delete_term(&pid, source))?;
                    }
                    Ok::<_, crate::client::Error>(())
                })
                .await;
            _ = this.update(cx, |view: &mut GlossaryView, cx| {
                match result {
                    Ok(()) => {
                        view.selected.clear();
                        view.refresh(cx);
                    }
                    Err(err) => {
                        view.error = Some(err.to_string());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn resolve(&mut self, cid: i64, decision: &str, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let pid = self.pid.clone();
        let decision = decision.to_string();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    block_on(client.resolve_conflict(&pid, cid, &decision, None))
                })
                .await;
            _ = this.update(cx, |view: &mut GlossaryView, cx| {
                match result {
                    Ok(_) => view.refresh(cx),
                    Err(err) => {
                        view.error = Some(err.to_string());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }
    /// Pick a CSV/JSON file (rfd, off the UI thread), parse it client-side,
    /// then either surface per-source overwrite/skip conflicts or import
    /// directly. Mirrors the React `ImportDialog` steps.
    fn pick_import_file(&mut self, cx: &mut Context<Self>) {
        let existing: std::collections::HashMap<String, String> = self
            .terms
            .iter()
            .map(|t| (t.source.clone(), t.target.clone()))
            .collect();
        cx.spawn(async move |this, cx| {
            let outcome: Result<ImportPicked, String> = cx
                .background_spawn(async move {
                    let path = rfd::FileDialog::new()
                        .add_filter("glossary", &["csv", "json"])
                        .pick_file()
                        .ok_or_else(|| "__cancelled__".to_string())?;
                    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
                    let is_csv = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| e.eq_ignore_ascii_case("csv"))
                        .unwrap_or(false);
                    let items = if is_csv {
                        parse_csv_terms(&text).map_err(|e| e.to_string())?
                    } else {
                        parse_json_terms(&text).map_err(|e| e.to_string())?
                    };
                    if items.is_empty() {
                        return Err("The file is empty or has no valid terms".to_string());
                    }
                    Ok(ImportPicked { items })
                })
                .await;
            _ = this.update(cx, |view: &mut GlossaryView, cx| {
                match outcome {
                    Err(cancelled) if cancelled == "__cancelled__" => {}
                    Err(err) => {
                        view.import_error = Some(err);
                        cx.notify();
                    }
                    Ok(picked) => {
                        view.import_error = None;
                        view.import_parsed = picked.items;
                        view.import_conflicts = view
                            .import_parsed
                            .iter()
                            .filter_map(|it| {
                                existing.get(&it.source).map(|current| ImportConflict {
                                    source: it.source.clone(),
                                    existing_target: current.clone(),
                                    new_target: it.target.clone(),
                                    decision: ImportDecision::Overwrite,
                                })
                            })
                            .collect();
                        if view.import_conflicts.is_empty() {
                            view.do_import(cx);
                        } else {
                            cx.notify();
                        }
                    }
                }
            });
        })
        .detach();
    }

    fn toggle_import_conflict(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(c) = self.import_conflicts.get_mut(index) {
            c.decision = if c.decision == ImportDecision::Skip {
                ImportDecision::Overwrite
            } else {
                ImportDecision::Skip
            };
        }
        cx.notify();
    }

    fn do_import(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let pid = self.pid.clone();
        let skip: std::collections::HashSet<String> = self
            .import_conflicts
            .iter()
            .filter(|c| c.decision == ImportDecision::Skip)
            .map(|c| c.source.clone())
            .collect();
        let terms: Vec<TermIn> = self
            .import_parsed
            .iter()
            .filter(|t| !skip.contains(&t.source))
            .cloned()
            .collect();
        self.importing = true;
        self.import_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { block_on(client.import_glossary(&pid, &terms)) })
                .await;
            _ = this.update(cx, |view: &mut GlossaryView, cx| {
                view.importing = false;
                match result {
                    Ok(_) => {
                        view.import_open = false;
                        view.import_conflicts.clear();
                        view.import_parsed.clear();
                        view.import_error = None;
                        view.refresh(cx);
                    }
                    Err(err) => {
                        view.import_error = Some(err.to_string());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Export via `export_glossary` bytes → rfd save dialog → write file.
    /// The blocking dialog + write run on a background thread.
    fn run_export(&mut self, format: &str, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let pid = self.pid.clone();
        let format = format.to_string();
        self.export_open = false;
        self.exporting = true;
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let saved: Result<String, String> = cx
                .background_spawn(async move {
                    let bytes = block_on(client.export_glossary(&pid, &format))
                        .map_err(|e| e.to_string())?;
                    let mut dialog = rfd::FileDialog::new()
                        .set_title("Save glossary")
                        .set_file_name(format!("glossary.{format}"));
                    dialog = dialog.add_filter("glossary", &[format.as_str()]);
                    let Some(dest) = dialog.save_file() else {
                        return Err("__cancelled__".into());
                    };
                    std::fs::write(&dest, &bytes).map_err(|e| e.to_string())?;
                    Ok(dest.to_string_lossy().into_owned())
                })
                .await;
            _ = this.update(cx, |view: &mut GlossaryView, cx| {
                view.exporting = false;
                match saved {
                    Ok(_) => view.refresh(cx),
                    Err(cancelled) if cancelled == "__cancelled__" => cx.notify(),
                    Err(err) => {
                        view.error = Some(err);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn toggle_select(&mut self, source: String, cx: &mut Context<Self>) {
        if !self.selected.remove(&source) {
            self.selected.insert(source);
        }
        cx.notify();
    }

    fn toggle_select_all(&mut self, cx: &mut Context<Self>) {
        if self.selected.len() == self.terms.len() {
            self.selected.clear();
        } else {
            self.selected = self.terms.iter().map(|t| t.source.clone()).collect();
        }
        cx.notify();
    }
}
/// Parsed-but-uncommitted import payload (kept small: terms only).
struct ImportPicked {
    items: Vec<TermIn>,
}

/// Parse the `glossary.export?format=csv` shape: header row + `|`-joined
/// aliases. Mirrors `parseCsv` in `GlossaryPage.tsx`.
fn parse_csv_terms(text: &str) -> Result<Vec<TermIn>, String> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut value = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            if quoted && chars.peek() == Some(&'"') {
                value.push('"');
                chars.next();
            } else {
                quoted = !quoted;
            }
        } else if !quoted && c == ',' {
            row.push(std::mem::take(&mut value));
        } else if !quoted && c == '\n' {
            if value.ends_with('\r') {
                value.pop();
            }
            row.push(std::mem::take(&mut value));
            rows.push(std::mem::take(&mut row));
        } else {
            value.push(c);
        }
    }
    if quoted {
        return Err("Unclosed quote in CSV".into());
    }
    if !value.is_empty() || !row.is_empty() {
        if value.ends_with('\r') {
            value.pop();
        }
        row.push(value);
        rows.push(row);
    }
    let Some(headers) = rows.first().cloned() else {
        return Ok(Vec::new());
    };
    let headers: Vec<String> = headers
        .iter()
        .map(|h| h.trim().trim_start_matches('\u{feff}').to_string())
        .collect();
    let mut out = Vec::new();
    for vals in rows.iter().skip(1) {
        if !vals.iter().any(|v| !v.is_empty()) {
            continue;
        }
        let mut get = |name: &str| -> String {
            headers
                .iter()
                .position(|h| h == name)
                .and_then(|i| vals.get(i))
                .cloned()
                .unwrap_or_default()
        };
        let source = get("source");
        let target = get("target");
        if source.is_empty() || target.is_empty() {
            continue;
        }
        out.push(TermIn {
            source,
            target,
            reading: get("reading"),
            r#type: {
                let t = get("type");
                if t.is_empty() { "term".into() } else { t }
            },
            gender: get("gender"),
            aliases: get("aliases")
                .split('|')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            note: get("note"),
        });
    }
    Ok(out)
}

/// Parse the `glossary.export?format=json` shape: a list of `{source,
/// target, …}` objects. Mirrors the React `JSON.parse` import path.
fn parse_json_terms(text: &str) -> Result<Vec<TermIn>, String> {
    let items: Vec<serde_json::Value> =
        serde_json::from_str(text).map_err(|_| "Could not parse JSON".to_string())?;
    let mut out = Vec::new();
    for item in &items {
        let source = item.get("source").and_then(|v| v.as_str()).unwrap_or("");
        let target = item.get("target").and_then(|v| v.as_str()).unwrap_or("");
        if source.is_empty() || target.is_empty() {
            return Err("The term file must be a list of {source, target} objects".into());
        }
        let aliases = match item.get("aliases") {
            Some(serde_json::Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect(),
            Some(serde_json::Value::String(s)) => s
                .split('|')
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        };
        let str_field = |name: &str, default: &str| {
            item.get(name)
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| default.into())
        };
        out.push(TermIn {
            source: source.into(),
            target: target.into(),
            reading: str_field("reading", ""),
            r#type: str_field("type", "term"),
            gender: str_field("gender", ""),
            aliases,
            note: str_field("note", ""),
        });
    }
    Ok(out)
}

impl Render for GlossaryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current_palette();
        let busy = self.busy();
        let mut col = div().flex().flex_col().flex_1().min_h_0().gap_3().p_6()
            .overflow_y_scrollbar().child(
            div().flex().items_center().justify_between().child(
                div().text_xl().font_weight(FontWeight::BOLD).child("Glossary"),
            ).child(
                div().flex().gap_2()
                    .child(
                        Button::new("glossary-export")
                            .label(if self.exporting { "Exporting…" } else { "Export" })
                            .disabled(self.exporting)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.export_open = !this.export_open;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("glossary-import")
                            .label("Import")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.import_open = true;
                                this.import_error = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("glossary-add")
                            .label("Add term")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.open_add(cx))),
                    ),
            ),
        );
        // Export format menu (mirrors the React CSV/JSON dropdown).
        if self.export_open {
            col = col.child(
                div().flex().gap_2().items_center()
                    .child(
                        Button::new("export-csv")
                            .label("CSV file")
                            .on_click(cx.listener(|this, _, _, cx| this.run_export("csv", cx))),
                    )
                    .child(
                        Button::new("export-json")
                            .label("JSON file")
                            .on_click(cx.listener(|this, _, _, cx| this.run_export("json", cx))),
                    )
                    .child(
                        Button::new("export-cancel").ghost().label("Cancel").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.export_open = false;
                                cx.notify();
                            }),
                        ),
                    ),
            );
        }

        if busy {
            col = col.child(
                div().text_sm().text_color(palette.muted_foreground)
                    .child("Project is running — glossary is read-only."),
            );
        }
        if let Some(err) = &self.error {
            col = col.child(
                div().p_3().rounded_md().bg(palette.destructive)
                    .text_color(palette.destructive_foreground)
                    .child(format!("Error: {err}")),
            );
        }

        // Search + type filter row.
        col = col.child(
            div().flex().gap_2()
                .child(div().flex_1().child(Input::new(&self.search)))
                .child(
                    Button::new("glossary-search")
                        .label("Search")
                        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                ),
        );
        let mut type_row = div().flex().gap_1().flex_wrap();
        type_row = type_row.child(filter_chip("", "All", &self.type_filter, palette, cx));
        for (id, label) in TERM_TYPES {
            type_row = type_row.child(filter_chip(id, label, &self.type_filter, palette, cx));
        }
        col = col.child(type_row);

        // Conflict queue.
        if !self.conflicts.is_empty() {
            let mut queue = div().flex().flex_col().gap_2().p_4().rounded_md()
                .border_1().border_color(palette.border).bg(palette.card)
                .child(div().font_medium().child(format!("Conflicts ({})", self.conflicts.len())));
            for conflict in self.conflicts.clone() {
                let cid = conflict.id;
                queue = queue.child(
                    div().flex().flex_col().gap_1().text_sm()
                        .child(div().font_medium().child(conflict.source.clone()))
                        .child(format!(
                            "current: {} → proposed: {}",
                            conflict.existing_target.as_deref().unwrap_or("—"),
                            conflict.proposed_target.as_deref().unwrap_or("—"),
                        ))
                        .child(
                            div().flex().gap_2()
                                .child(
                                    Button::new(format!("keep-{cid}"))
                                        .label("Keep current")
                                        .disabled(busy)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.resolve(cid, "current", cx);
                                        })),
                                )
                                .child(
                                    Button::new(format!("accept-{cid}"))
                                        .primary()
                                        .label("Accept suggestion")
                                        .disabled(busy)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.resolve(cid, "proposed", cx);
                                        })),
                                ),
                        ),
                );
            }
            col = col.child(queue);
        }

        // Batch bar.
        if !self.selected.is_empty() {
            col = col.child(
                div().flex().items_center().gap_2()
                    .child(format!("{} selected", self.selected.len()))
                    .child(
                        Button::new("batch-delete")
                            .label("Delete selected")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.delete_selected(window, cx);
                            })),
                    )
                    .child(
                        Button::new("batch-clear")
                            .ghost()
                            .label("Clear")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.selected.clear();
                                cx.notify();
                            })),
                    ),
            );
        }

        // Terms list (table semantics: source/target/type/actions + checkbox).
        if self.loading && self.terms.is_empty() {
            col = col.child(div().text_sm().text_color(palette.muted_foreground).child("Loading…"));
        } else if self.terms.is_empty() {
            col = col.child(
                div().py_12().text_center().text_sm().text_color(palette.muted_foreground)
                    .child("No terms yet."),
            );
        } else {
            let all_selected = !self.terms.is_empty() && self.selected.len() == self.terms.len();
            let mut list = div().flex().flex_col().gap_1();
            list = list.child(
                div().flex().items_center().gap_2().text_xs()
                    .font_medium().text_color(palette.muted_foreground)
                    .child(
                        Checkbox::new("select-all")
                            .checked(all_selected)
                            .on_change(cx.listener(|this, _, _, cx| this.toggle_select_all(cx))),
                    )
                    .child(div().w_40().child("Source"))
                    .child(div().flex_1().child("Target"))
                    .child(div().w_32().child("Type")),
            );
            let filtered_count = self.filtered.len();
            let list_rows = uniform_list(
                "glossary-terms",
                filtered_count,
                cx.processor(|this, range, _window, cx| {
                    let busy = this.busy();
                    let palette = theme::current_palette();
                    let mut items = Vec::new();
                    for ix in range {
                        let term: &crate::client::GlossaryTerm = &this.filtered[ix];
                        let source = term.source.clone();
                        let source_edit = term.source.clone();
                        let checked = this.selected.contains(&source);
                        let row = div()
                            .h(px(40.))
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .p_2()
                            .rounded_md()
                            .border_1()
                            .border_color(palette.border)
                            .child(
                                Checkbox::new(format!("sel-{}", source))
                                    .checked(checked)
                                    .on_change(cx.listener(move |this, _, _, cx| {
                                        this.toggle_select(source.clone(), cx);
                                    })),
                            )
                            .child(div().w_40().font_medium().truncate().child(term.source.clone()))
                            .child(div().flex_1().truncate().child(term.target.clone()))
                            .child(div().w_32().text_xs().truncate().child(term_type_label(&term.r#type).to_string()))
                            .child(
                                Button::new(format!("edit-{}", source_edit))
                                    .ghost()
                                    .label("Edit")
                                    .disabled(busy)
                                    .on_click({
                                        let term = term.clone();
                                        cx.listener(move |this, _, _, cx| this.open_edit(&term, cx))
                                    }),
                            )
                            .child(
                                Button::new(format!("del-{}", term.source))
                                    .ghost()
                                    .label("Delete")
                                    .disabled(busy)
                                    .on_click({
                                        let source = term.source.clone();
                                        cx.listener(move |this, _, _, cx| this.delete_one(source.clone(), cx))
                                    }),
                            );
                        items.push(row);
                    }
                    items
                }),
            )
            .h(px(400.));
            list = list.child(list_rows);
            col = col.child(list);
        }

        // Add/Edit dialog with real field inputs (inline panel; modal Dialog
        // lands with the dialog pass). Entities are created once per open and
        // seed from `form`; values sync back on save.
        if let Some(edit_key) = self.editing.clone() {
            let title = if edit_key.is_some() { "Edit term" } else { "Add term" };
            if self.form_source.is_none() {
                let v = self.form.source.clone();
                self.form_source = Some(cx.new(|cx| {
                    let mut s = InputState::new(window, cx).placeholder("Source term");
                    s.set_value(v, window, cx);
                    s
                }));
            }
            if self.form_target.is_none() {
                let v = self.form.target.clone();
                self.form_target = Some(cx.new(|cx| {
                    let mut s = InputState::new(window, cx).placeholder("Translated term");
                    s.set_value(v, window, cx);
                    s
                }));
            }
            if self.form_reading.is_none() {
                let v = self.form.reading.clone();
                self.form_reading = Some(cx.new(|cx| {
                    let mut s = InputState::new(window, cx).placeholder("Reading");
                    s.set_value(v, window, cx);
                    s
                }));
            }
            if self.form_gender.is_none() {
                let v = self.form.gender.clone();
                self.form_gender = Some(cx.new(|cx| {
                    let mut s = InputState::new(window, cx).placeholder("Unspecified");
                    s.set_value(v, window, cx);
                    s
                }));
            }
            if self.form_aliases.is_none() {
                let v = self.form.aliases.join(", ");
                self.form_aliases = Some(cx.new(|cx| {
                    let mut s = InputState::new(window, cx).placeholder("alias1, alias2");
                    s.set_value(v, window, cx);
                    s
                }));
            }
            if self.form_note.is_none() {
                let v = self.form.note.clone();
                self.form_note = Some(cx.new(|cx| {
                    let mut s = InputState::new(window, cx).placeholder("Notes");
                    s.set_value(v, window, cx);
                    s
                }));
            }
            // Type select (options mirror TERM_TYPES; writes back via Confirm).
            let type_opts: Vec<String> = TERM_TYPES.iter().map(|(id, _)| id.to_string()).collect();
            if self.form_type_select.is_none() || self.form_type_options != type_opts {
                let cur = if self.form.r#type.is_empty() { "term".into() } else { self.form.r#type.clone() };
                let idx = type_opts.iter().position(|o| o == &cur).unwrap_or(0);
                let opts = type_opts.clone();
                let entity = cx.new(|cx| {
                    SelectState::new(opts, Some(IndexPath::new(idx)), window, cx)
                });
                cx.subscribe(
                    &entity,
                    |this: &mut GlossaryView, _, event: &SelectEvent<Vec<String>>, _| {
                        if let SelectEvent::Confirm(Some(value)) = event {
                            this.form.r#type = value.clone();
                        }
                    },
                )
                .detach();
                self.form_type_select = Some(entity);
                self.form_type_options = type_opts;
            }
            let mut panel = div().flex().flex_col().gap_2().p_4().rounded_md()
                .border_1().border_color(palette.ring).bg(palette.card)
                .on_key_down(cx.listener(|this, ev: &gpui_kit::KeyDownEvent, _, cx| {
                    // Esc closes the term editor (dialogs also close on Esc
                    // via the kit default); Tab order stays native.
                    if ev.keystroke.key == "escape" {
                        this.close_form(cx);
                    }
                }))
                .child(div().font_medium().child(title))
                .child(div().flex().flex_col().gap_1().child("Source term").child(Input::new(self.form_source.as_ref().unwrap())))
                .child(div().flex().flex_col().gap_1().child("Translated term").child(Input::new(self.form_target.as_ref().unwrap())))
                .child(div().flex().flex_col().gap_1().child("Reading").child(Input::new(self.form_reading.as_ref().unwrap())))
                .child(div().flex().flex_col().gap_1().child("Type").child(Select::new(self.form_type_select.as_ref().unwrap())))
                .child(div().flex().flex_col().gap_1().child("Gender (unspecified / male / female / other / unknown)").child(Input::new(self.form_gender.as_ref().unwrap())))
                .child(div().flex().flex_col().gap_1().child("Aliases (comma-separated)").child(Input::new(self.form_aliases.as_ref().unwrap())))
                .child(div().flex().flex_col().gap_1().child("Notes").child(Input::new(self.form_note.as_ref().unwrap())))
                .child(
                    div().flex().gap_2()
                        .child(
                            Button::new("save-term")
                                .primary()
                                .label(if self.saving { "Saving…" } else { "Save" })
                                .disabled(self.saving)
                                .on_click(cx.listener(|this, _, _, cx| this.save_form(cx))),
                        )
                        .child(
                            Button::new("cancel-term").ghost().label("Cancel").on_click(
                                cx.listener(|this, _, _, cx| this.close_form(cx)),
                            ),
                        ),
                );
            if let Some(err) = &self.form_error {
                panel = panel.child(div().text_sm().text_color(palette.destructive).child(err.clone()));
            }
            col = col.child(panel);
        }

        // Import panel (mirrors the React ImportDialog: upload → conflicts → importing).
        if self.import_open {
            let mut panel = div().flex().flex_col().gap_2().p_4().rounded_md()
                .border_1().border_color(palette.ring).bg(palette.card)
                .on_key_down(cx.listener(|this, ev: &gpui_kit::KeyDownEvent, _, cx| {
                    // Esc closes the import panel.
                    if ev.keystroke.key == "escape" {
                        this.import_open = false;
                        this.import_error = None;
                        cx.notify();
                    }
                }))
                .child(div().font_medium().child("Import terms"));
            if let Some(err) = &self.import_error {
                panel = panel.child(div().text_sm().text_color(palette.destructive).child(err.clone()));
            }
            if self.importing {
                panel = panel.child(div().text_sm().text_color(palette.muted_foreground).child("Importing…"));
            } else if self.import_conflicts.is_empty() {
                panel = panel
                    .child(div().text_sm().text_color(palette.muted_foreground).child(
                        "Import terms from CSV or JSON. Existing sources become overwrite/skip conflicts.",
                    ))
                    .child(
                        div().flex().gap_2()
                            .child(
                                Button::new("pick-import-file")
                                    .label("Choose file")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| this.pick_import_file(cx))),
                            )
                            .child(
                                Button::new("cancel-import").ghost().label("Cancel").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.import_open = false;
                                        this.import_error = None;
                                        cx.notify();
                                    }),
                                ),
                            ),
                    );
            } else {
                let count = self.import_conflicts.len();
                panel = panel.child(div().text_sm().child(format!(
                    "{count} terms already exist — choose overwrite or skip each."
                )));
                let conflicts = self.import_conflicts.clone();
                let mut list = div().flex().flex_col().gap_1();
                for (i, c) in conflicts.iter().enumerate() {
                    let label = if c.decision == ImportDecision::Overwrite { "Overwrite" } else { "Skip" };
                    list = list.child(
                        div().flex().items_center().gap_2().text_sm().p_2().rounded_md()
                            .border_1().border_color(palette.border)
                            .child(div().font_medium().child(c.source.clone()))
                            .child(div().text_xs().text_color(palette.muted_foreground).child(format!(
                                "current: {} · imported: {}",
                                c.existing_target, c.new_target
                            )))
                            .child(
                                Button::new(format!("import-conflict-{i}"))
                                    .label(label)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.toggle_import_conflict(i, cx);
                                    })),
                            ),
                    );
                }
                panel = panel.child(list);
                panel = panel.child(
                    div().flex().gap_2()
                        .child(
                            Button::new("confirm-import")
                                .primary()
                                .label("Confirm import")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| this.do_import(cx))),
                        )
                        .child(
                            Button::new("cancel-import").ghost().label("Cancel").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.import_open = false;
                                    this.import_conflicts.clear();
                                    this.import_parsed.clear();
                                    this.import_error = None;
                                    cx.notify();
                                }),
                            ),
                        ),
                );
            }
            col = col.child(panel);
        }
        col
    }
}

/// Type filter chip — mirrors the Select options without a dropdown (U-pass).
fn filter_chip(
    id: &str,
    label: &str,
    active: &str,
    _palette: theme::Palette,
    cx: &mut gpui_kit::Context<GlossaryView>,
) -> impl IntoElement {
    let id = id.to_string();
    let mut btn = Button::new(format!("type-{id}")).label(label.to_string());
    if active == id {
        btn = btn.primary();
    } else {
        btn = btn.ghost();
    }
    btn.on_click(cx.listener(move |this: &mut GlossaryView, _, _, cx| {
        this.type_filter = id.clone();
        this.refresh(cx);
    }))
}

#[cfg(test)]
mod tests {
    use super::{parse_csv_terms, parse_json_terms};

    #[test]
    fn csv_parses_headers_and_alias_pipes() {
        let text = "source,target,reading,type,gender,aliases,note\npomme,apple,,term,,a|b,n\n";
        let terms = parse_csv_terms(text).unwrap();
        assert_eq!(terms.len(), 1);
        assert_eq!(terms[0].source, "pomme");
        assert_eq!(terms[0].target, "apple");
        assert_eq!(terms[0].aliases, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn csv_rejects_unclosed_quote_and_skips_blank_sources() {
        assert!(parse_csv_terms("source,target\n\"abc,def\n").is_err());
        let terms = parse_csv_terms("source,target\n,apple\nx,y\n").unwrap();
        assert_eq!(terms.len(), 1);
        assert_eq!(terms[0].source, "x");
    }

    #[test]
    fn json_parses_and_rejects_non_term_rows() {
        let ok = parse_json_terms(r#"[{"source":"a","target":"b"}]"#).unwrap();
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].r#type, "term");
        assert!(parse_json_terms(r#"[{"source":"a"}]"#).is_err());
        assert!(parse_json_terms("nope").is_err());
    }
}
