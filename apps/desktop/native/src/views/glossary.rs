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
//!
//! Term types mirror `termTypes()` in the source.

use std::collections::HashSet;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{Disableable, IndexPath, StyledExt};
use gpui_kit::*;

use crate::{
    service::{
        is_project_busy, CuratorStatus, GlossaryTerm, ServiceError, ServiceHandle, TermConflict,
        TermIn,
    },
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
    TERM_TYPES
        .iter()
        .find(|(id, _)| *id == t)
        .map(|(_, l)| *l)
        .unwrap_or(t)
}

/// Tag badges for a term row: mention-count ranking + conflict flag.
/// Pure display from cached fields (no backend round-trip per row).
fn term_tags(term: &GlossaryTerm, palette: &theme::Palette) -> impl IntoElement {
    let mut tags = div().flex().flex_none().items_center().gap_1();
    if term.mentions > 0 {
        let label = if term.mentions >= 50 {
            format!("★ {}×", term.mentions)
        } else {
            format!("{}×", term.mentions)
        };
        tags = tags.child(
            div()
                .text_xs()
                .px_1p5()
                .py_0p5()
                .rounded_sm()
                .bg(palette.muted)
                .text_color(palette.muted_foreground)
                .child(label),
        );
    }
    if term.status == "conflict" {
        tags = tags.child(
            div()
                .text_xs()
                .px_1p5()
                .py_0p5()
                .rounded_sm()
                .bg(palette.destructive.opacity(0.2))
                .text_color(palette.destructive)
                .child("conflict"),
        );
    }
    tags
}

/// Workspace tab: browse terms (default), review conflicts, or worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum GlossaryTab {
    #[default]
    Browse,
    Conflicts,
    Worker,
}

pub struct GlossaryView {
    service: Option<ServiceHandle>,
    pid: String,
    visible: bool,
    project_status: String,
    terms: Vec<GlossaryTerm>,
    /// Cached filtered view; recomputed only on terms/search/type_filter change.
    filtered: Vec<GlossaryTerm>,
    /// Cached per-type counts over the FULL glossary for the stats footer
    /// (independent of search/filters); recomputed on data load.
    type_counts: Vec<(String, usize)>,
    conflicts: Vec<TermConflict>,
    /// Virtualized list states (terms + conflicts queues).
    terms_list: ListState,
    conflicts_list: ListState,
    /// Active workspace tab (Browse default; Conflicts highlighted red).
    tab: GlossaryTab,
    loading: bool,
    error: Option<String>,
    /// Signature of the last applied (terms, conflicts, status) triple;
    /// `refresh` skips resets + notify when a reload is unchanged.
    data_sig: u64,
    search: Entity<InputState>,
    search_text: String,
    type_filter: String,
    selected: HashSet<String>,
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
    curator: Option<CuratorStatus>,
    curator_loading: bool,
    curator_error: Option<String>,
    /// Search debounce token: typing re-arms a 150ms timer, so fast typing
    /// filters once instead of cloning the term set per keystroke.
    search_token: u64,
}
impl GlossaryView {
    pub fn new(pid: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search terms…"));
        // Live local filtering, debounced: typing re-arms a 150ms timer so
        // a burst filters once (per-keystroke clones stuttered at 3k terms).
        cx.subscribe(&search, |this: &mut GlossaryView, _, _: &InputEvent, cx| {
            this.search_token = this.search_token.wrapping_add(1);
            let token = this.search_token;
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(150))
                    .await;
                _ = this.update(cx, |view: &mut GlossaryView, cx| {
                    if view.search_token == token {
                        view.apply_filter(cx);
                    }
                });
            })
            .detach();
        })
        .detach();
        Self {
            service: None,
            pid,
            visible: true,
            project_status: String::new(),
            terms: Vec::new(),
            filtered: Vec::new(),
            type_counts: Vec::new(),
            conflicts: Vec::new(),
            terms_list: ListState::new(0, ListAlignment::Top, px(200.0)),
            conflicts_list: ListState::new(0, ListAlignment::Top, px(200.0)),
            tab: GlossaryTab::Browse,
            loading: true,
            error: None,
            data_sig: 0,
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
            curator: None,
            curator_loading: false,
            curator_error: None,
            search_token: 0,
        }
    }

    pub fn set_client(&mut self, service: ServiceHandle, cx: &mut Context<Self>) {
        self.service = Some(service);
        self.refresh(cx);
    }

    /// Called by the App shell on navigation. Becoming visible refreshes.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if visible && !self.visible {
            self.visible = true;
            self.refresh(cx);
        } else {
            self.visible = visible;
        }
    }

    fn busy(&self) -> bool {
        is_project_busy(&self.project_status)
    }

    /// Signature of the (terms, conflicts, status) triple for the refresh
    /// no-op guard: term sources + targets, conflict ids + targets, and
    /// the project status.
    fn data_signature(terms: &[GlossaryTerm], conflicts: &[TermConflict], status: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        terms.len().hash(&mut hasher);
        for t in terms {
            t.source.hash(&mut hasher);
            t.target.hash(&mut hasher);
            t.term_type.hash(&mut hasher);
        }
        conflicts.len().hash(&mut hasher);
        for c in conflicts {
            c.id.hash(&mut hasher);
            c.existing_target.hash(&mut hasher);
            c.proposed_target.hash(&mut hasher);
        }
        status.hash(&mut hasher);
        hasher.finish()
    }
    /// Recompute the filtered view from loaded terms + current search text
    /// and type filter. Local and cheap (no backend round-trip); called on
    /// every keystroke, chip click, and after data loads.
    fn apply_filter(&mut self, cx: &mut Context<Self>) {
        self.search_text = self.search.read(cx).value().to_string();
        let q = self.search_text.trim().to_lowercase();
        self.filtered = self
            .terms
            .iter()
            .filter(|t| {
                (self.type_filter.is_empty() || t.term_type == self.type_filter)
                    && (q.is_empty()
                        || t.source.to_lowercase().contains(&q)
                        || t.target.to_lowercase().contains(&q)
                        || t.aliases.iter().any(|a| a.to_lowercase().contains(&q)))
            })
            .cloned()
            .collect();
        self.terms_list.reset(self.filtered.len());
        cx.notify();
    }

    /// Recompute per-type counts over the FULL term set (stats footer input).
    /// Called on data load only — footer stays stable under search/filters.
    fn recompute_type_counts(&mut self) {
        let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
        for t in &self.terms {
            let key = if t.term_type.trim().is_empty() {
                "uncategorized".to_string()
            } else {
                term_type_label(&t.term_type).to_string()
            };
            *counts.entry(key).or_default() += 1;
        }
        let mut out: Vec<(String, usize)> = counts.into_iter().collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        self.type_counts = out;
    }

    /// One virtualized term row (checkbox + source/target/type + actions).
    /// Reads by index — only visible rows are built.
    fn term_row(
        view: &GlossaryView,
        ix: usize,
        palette: theme::Palette,
        cx: &mut Context<GlossaryView>,
    ) -> AnyElement {
        let Some(term) = view.filtered.get(ix) else {
            return div().into_any_element();
        };
        let busy = view.busy();
        let source = term.source.clone();
        let source_edit = term.source.clone();
        let checked = view.selected.contains(&source);
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_sm()
            .px_2()
            .py_1()
            .child(
                Checkbox::new(format!("sel-{source}"))
                    .checked(checked)
                    .on_change(cx.listener(move |this, _, _, cx| {
                        this.toggle_select(source.clone(), cx);
                    })),
            )
            .child(
                div()
                    .w_40()
                    .font_medium()
                    .truncate()
                    .child(term.source.clone()),
            )
            .child(div().flex_1().truncate().child(term.target.clone()))
            .child(
                div()
                    .w_32()
                    .text_xs()
                    .truncate()
                    .child(term_type_label(&term.term_type).to_string()),
            )
            .child(term_tags(term, &palette))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .gap_1()
                    .child(
                        Button::new(format!("edit-{source_edit}"))
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
                                cx.listener(move |this, _, _, cx| {
                                    this.delete_one(source.clone(), cx)
                                })
                            }),
                    ),
            )
            .into_any_element()
    }

    /// One virtualized conflict row (source + current/proposed + actions).
    fn conflict_row(
        view: &GlossaryView,
        ix: usize,
        palette: theme::Palette,
        cx: &mut Context<GlossaryView>,
    ) -> AnyElement {
        let Some(conflict) = view.conflicts.get(ix) else {
            return div().into_any_element();
        };
        let busy = view.busy();
        let cid = conflict.id;
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .px_2()
            .py_2()
            .text_sm()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .truncate()
                            .child(conflict.source.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(palette.muted_foreground)
                            .truncate()
                            .child(format!(
                                "current: {} → proposed: {}",
                                conflict.existing_target.as_deref().unwrap_or("—"),
                                conflict.proposed_target.as_deref().unwrap_or("—"),
                            )),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .flex_none()
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
            )
            .into_any_element()
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let (Some(service), pid) = (self.service.clone(), self.pid.clone()) else {
            return;
        };
        // Fetch the full term set; search/type filtering happens locally in
        // `apply_filter` (instant, no refetch per keystroke).
        self.loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    // Sequential, not join!: terms/conflicts/project render
                    // independently; one hanging endpoint must not blank all.
                    let terms = service.list_terms(&pid)?;
                    let conflicts = service.list_conflicts(&pid)?;
                    let project = service.get_project(&pid)?;
                    Ok::<_, ServiceError>((terms, conflicts, project.project.status))
                })
                .await;
            _ = this.update(cx, |view: &mut GlossaryView, cx| {
                view.loading = false;
                match result {
                    Ok((terms, conflicts, status)) => {
                        // No-op guard: an unchanged reload skips resets +
                        // notify (the refresh path had none, unlike the
                        // filter path). Signature covers term identities +
                        // targets, conflict ids, and project status.
                        let sig = Self::data_signature(&terms, &conflicts, &status);
                        if sig != view.data_sig {
                            view.data_sig = sig;
                            view.terms = terms;
                            view.conflicts = conflicts;
                            view.conflicts_list.reset(view.conflicts.len());
                            view.project_status = status;
                            view.recompute_type_counts();
                            // Drop selections for vanished terms.
                            view.selected
                                .retain(|s| view.terms.iter().any(|t| &t.source == s));
                            view.apply_filter(cx);
                            return;
                        }
                    }
                    Err(err) => {
                        let msg = err.to_string();
                        // Same guard for errors: repeated failures notify once.
                        if view.error.as_ref() != Some(&msg) {
                            view.error = Some(msg);
                            cx.notify();
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn refresh_curator(&mut self, cx: &mut Context<Self>) {
        let (Some(service), pid) = (self.service.clone(), self.pid.clone()) else {
            return;
        };
        self.curator_loading = true;
        self.curator_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { service.curator_status(&pid) })
                .await;
            _ = this.update(cx, |view: &mut GlossaryView, cx| {
                view.curator_loading = false;
                match result {
                    Ok(status) => view.curator = Some(status),
                    Err(err) => view.curator_error = Some(err.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        // Live poll while the Worker tab is open and a pass runs; only
        // re-arms while visible (navigation resumes via `set_visible`).
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(3))
                .await;
            _ = this.update(cx, |view: &mut GlossaryView, cx| {
                let running = view.visible
                    && view.tab == GlossaryTab::Worker
                    && view.curator.as_ref().is_some_and(|s| s.running);
                if running {
                    view.refresh_curator(cx);
                }
            });
        })
        .detach();
    }

    fn curator_start(&mut self, cx: &mut Context<Self>) {
        let (Some(service), pid) = (self.service.clone(), self.pid.clone()) else {
            return;
        };
        self.curator_loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { service.curator_start(&pid) })
                .await;
            _ = this.update(cx, |view: &mut GlossaryView, cx| {
                if let Err(err) = result {
                    view.curator_loading = false;
                    view.curator_error = Some(err.to_string());
                    cx.notify();
                } else {
                    view.refresh_curator(cx);
                }
            });
        })
        .detach();
    }

    fn curator_stop(&mut self, cx: &mut Context<Self>) {
        let (Some(service), pid) = (self.service.clone(), self.pid.clone()) else {
            return;
        };
        self.curator_loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { service.curator_stop(&pid) })
                .await;
            _ = this.update(cx, |view: &mut GlossaryView, cx| {
                if let Err(err) = result {
                    view.curator_loading = false;
                    view.curator_error = Some(err.to_string());
                    cx.notify();
                } else {
                    view.refresh_curator(cx);
                }
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
            term_type: if term.term_type.is_empty() {
                "term".into()
            } else {
                term.term_type.clone()
            },
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
        let Some(service) = self.service.clone() else {
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
                        Some(source) => {
                            service.update_term(&pid, &source, form.clone()).map(|_| ())
                        }
                        None => service.add_term(&pid, form.clone()).map(|_| ()),
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
        let Some(service) = self.service.clone() else {
            return;
        };
        let pid = self.pid.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { service.delete_term(&pid, &source) })
                .await;
            _ = this.update(cx, |view: &mut GlossaryView, cx| match result {
                Ok(_) => view.refresh(cx),
                Err(err) => {
                    view.error = Some(err.to_string());
                    cx.notify();
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
        let (Some(service), pid) = (self.service.clone(), self.pid.clone()) else {
            return;
        };
        let sources: Vec<String> = self.selected.iter().cloned().collect();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    for source in &sources {
                        service.delete_term(&pid, source)?;
                    }
                    Ok::<_, ServiceError>(())
                })
                .await;
            _ = this.update(cx, |view: &mut GlossaryView, cx| match result {
                Ok(()) => {
                    view.selected.clear();
                    view.refresh(cx);
                }
                Err(err) => {
                    view.error = Some(err.to_string());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn resolve(&mut self, cid: i64, decision: &str, cx: &mut Context<Self>) {
        let Some(service) = self.service.clone() else {
            return;
        };
        let pid = self.pid.clone();
        // New engine takes the chosen final rendering as `target`: "current"
        // keeps the existing target, "proposed" accepts the suggestion.
        let target = self
            .conflicts
            .iter()
            .find(|c| c.id == cid)
            .map(|c| {
                if decision == "proposed" {
                    c.proposed_target.clone().unwrap_or_default()
                } else {
                    c.existing_target.clone().unwrap_or_default()
                }
            })
            .unwrap_or_default();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { service.resolve_conflict(&pid, cid, &target) })
                .await;
            _ = this.update(cx, |view: &mut GlossaryView, cx| match result {
                Ok(_) => view.refresh(cx),
                Err(err) => {
                    view.error = Some(err.to_string());
                    cx.notify();
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
impl Render for GlossaryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current_palette();
        let busy = self.busy();
        // Fixed workspace: header + tabs + search + filters pinned; only the
        // central table scrolls; stats footer pinned at the bottom. The page
        // keeps its scroll container so the flex chain resolves heights.
        let mut col = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_3()
            .p_6()
            .overflow_y_scrollbar()
            .font(theme::fonts::ui())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(super::ui::page_title("Glossary", None::<Div>))
                    .child(
                        div().flex().gap_2().child(
                            Button::new("glossary-add")
                                .label("Add term")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| this.open_add(cx))),
                        ),
                    ),
            );
        // Workspace tabs: Browse Terms default; Conflicts red; Worker.
        let conflict_count = self.conflicts.len();
        let tab_index = match self.tab {
            GlossaryTab::Browse => 0,
            GlossaryTab::Conflicts => 1,
            GlossaryTab::Worker => 2,
        };
        col = col.child(
            TabBar::new("glossary-tabs")
                .underline()
                .selected_index(tab_index)
                .on_click(cx.listener(|this, index: &usize, _, cx| {
                    this.tab = match *index {
                        1 => GlossaryTab::Conflicts,
                        2 => {
                            if this.curator.is_none() && !this.curator_loading {
                                this.refresh_curator(cx);
                            }
                            GlossaryTab::Worker
                        }
                        _ => GlossaryTab::Browse,
                    };
                    cx.notify();
                }))
                .child(Tab::new().label("Browse Terms"))
                .child(
                    Tab::new()
                        .label(format!("Conflicts ({conflict_count})"))
                        .text_color(palette.destructive),
                )
                .child(Tab::new().label("Worker")),
        );

        if busy {
            col = col.child(
                div()
                    .text_sm()
                    .text_color(palette.muted_foreground)
                    .child("Project is running — glossary is read-only."),
            );
        }
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

        // Search + type filters (borderless; tabs + search + filters pinned).
        let mut type_row = div().flex().gap_1().flex_wrap();
        type_row = type_row.child(filter_chip("", "All", &self.type_filter, palette, cx));
        for (id, label) in TERM_TYPES {
            type_row = type_row.child(filter_chip(id, label, &self.type_filter, palette, cx));
        }
        let search_block = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(Input::new(&self.search))
            .child(type_row);
        if self.tab == GlossaryTab::Browse {
            col = col.child(search_block);
        }

        // Conflicts tab: the review queue (red tab above); browse content hidden.
        if self.tab == GlossaryTab::Conflicts {
            col = col.child(conflicts_panel(self, &palette, cx));
        }
        // Worker tab: background curator status + controls.
        if self.tab == GlossaryTab::Worker {
            col = col.child(worker_panel(self, &palette, cx));
        }
        if self.tab == GlossaryTab::Browse {
            // Add/Edit form directly under the search card (immediately visible;
            // previously appended below the list, off-screen). Entities are
            // created once per open and seed from `form`; values sync back on save.
            if let Some(edit_key) = self.editing.clone() {
                let title = if edit_key.is_some() {
                    "Edit term"
                } else {
                    "Add term"
                };
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
                let type_opts: Vec<String> =
                    TERM_TYPES.iter().map(|(id, _)| id.to_string()).collect();
                if self.form_type_select.is_none() || self.form_type_options != type_opts {
                    let cur = if self.form.term_type.is_empty() {
                        "term".into()
                    } else {
                        self.form.term_type.clone()
                    };
                    let idx = type_opts.iter().position(|o| o == &cur).unwrap_or(0);
                    let opts = type_opts.clone();
                    let entity =
                        cx.new(|cx| SelectState::new(opts, Some(IndexPath::new(idx)), window, cx));
                    cx.subscribe(
                        &entity,
                        |this: &mut GlossaryView, _, event: &SelectEvent<Vec<String>>, _| {
                            if let SelectEvent::Confirm(Some(value)) = event {
                                this.form.term_type = value.clone();
                            }
                        },
                    )
                    .detach();
                    self.form_type_select = Some(entity);
                    self.form_type_options = type_opts;
                }
                let mut panel = div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_4()
                    .rounded_md()
                    .border_1()
                    .border_color(palette.ring)
                    .bg(palette.card)
                    .on_key_down(cx.listener(|this, ev: &gpui_kit::KeyDownEvent, _, cx| {
                        // Esc closes the term editor (dialogs also close on Esc
                        // via the kit default); Tab order stays native.
                        if ev.keystroke.key == "escape" {
                            this.close_form(cx);
                        }
                    }))
                    .child(div().font_medium().child(title))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child("Source term")
                            .child(Input::new(self.form_source.as_ref().unwrap())),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child("Translated term")
                            .child(Input::new(self.form_target.as_ref().unwrap())),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child("Reading")
                            .child(Input::new(self.form_reading.as_ref().unwrap())),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child("Type")
                            .child(Select::new(self.form_type_select.as_ref().unwrap())),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child("Gender (unspecified / male / female / other / unknown)")
                            .child(Input::new(self.form_gender.as_ref().unwrap())),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child("Aliases (comma-separated)")
                            .child(Input::new(self.form_aliases.as_ref().unwrap())),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child("Notes")
                            .child(Input::new(self.form_note.as_ref().unwrap())),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("save-term")
                                    .primary()
                                    .label(if self.saving { "Saving…" } else { "Save" })
                                    .disabled(self.saving)
                                    .on_click(cx.listener(|this, _, _, cx| this.save_form(cx))),
                            )
                            .child(
                                Button::new("cancel-term")
                                    .ghost()
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| this.close_form(cx))),
                            ),
                    );
                if let Some(err) = &self.form_error {
                    panel = panel.child(
                        div()
                            .text_sm()
                            .text_color(palette.destructive)
                            .child(err.clone()),
                    );
                }
                col = col.child(panel);
            }
            // Batch bar.
            if !self.selected.is_empty() {
                col = col.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(format!("{} selected", self.selected.len()))
                        .child(
                            Button::new("batch-delete")
                                .label("Delete selected")
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.delete_selected(window, cx);
                                })),
                        )
                        .child(Button::new("batch-clear").ghost().label("Clear").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.selected.clear();
                                cx.notify();
                            }),
                        )),
                );
            }

            // Terms list card: header row + virtualized rows filling the available
            // space (no fixed 400px box nested in the page scroll — the list owns
            // its scroll region like the proofreading segment list).
            if self.loading && self.terms.is_empty() {
                col = col.child(
                    div()
                        .text_sm()
                        .text_color(palette.muted_foreground)
                        .child("Loading…"),
                );
            } else if self.terms.is_empty() {
                col = col.child(
                    div()
                        .py_12()
                        .text_center()
                        .text_sm()
                        .text_color(palette.muted_foreground)
                        .child("No terms yet."),
                );
            } else if self.filtered.is_empty() {
                col = col.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .p_4()
                        .rounded_md()
                        .border_1()
                        .border_color(palette.border)
                        .bg(palette.card)
                        .child(div().font_weight(FontWeight::MEDIUM).child("Terms"))
                        .child(
                            div()
                                .py_8()
                                .text_center()
                                .text_sm()
                                .text_color(palette.muted_foreground)
                                .child("No terms match the current search or filter."),
                        ),
                );
            } else {
                let all_selected =
                    !self.terms.is_empty() && self.selected.len() == self.terms.len();
                let filtered_count = self.filtered.len();
                let total_count = self.terms.len();
                let view = cx.entity();
                let list_rows = list(self.terms_list.clone(), move |ix, _window, cx: &mut App| {
                    view.update(cx, |this, cx| Self::term_row(this, ix, palette, cx))
                })
                .flex_1()
                .min_h_0();
                col = col.child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_h_0()
                        .gap_1()
                        .pt_1()
                        .border_t_1()
                        .border_color(palette.border)
                        .overflow_hidden()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(format!("Terms ({filtered_count})")),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(palette.muted_foreground)
                                        .child(format!("{filtered_count} of {total_count} terms",)),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_xs()
                                .px_2()
                                .font_medium()
                                .text_color(palette.muted_foreground)
                                .child(Checkbox::new("select-all").checked(all_selected).on_change(
                                    cx.listener(|this, _, _, cx| this.toggle_select_all(cx)),
                                ))
                                .child(div().w_40().child("Source"))
                                .child(div().flex_1().child("Target"))
                                .child(div().w_32().child("Type")),
                        )
                        .child(list_rows),
                );
            }
        } // end Browse tab content (form + batch + list)
          // Fixed stats footer (always visible): category breakdown bar over
          // the full glossary, from cached `type_counts`.
        col = col.child(stats_footer(&self.type_counts, self.terms.len(), &palette));
        col
    }
}

/// One status card (dot + label + value).
fn worker_stat(
    label: &str,
    value: &str,
    dot: gpui_kit::Hsla,
    palette: &theme::Palette,
) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().w(px(8.0)).h(px(8.0)).rounded_full().bg(dot))
                .child(
                    div()
                        .text_xs()
                        .text_color(palette.muted_foreground)
                        .child(label.to_string()),
                ),
        )
        .child(
            div()
                .text_xl()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(value.to_string()),
        )
}

/// Background curator worker tab: status cards, start/stop, pass history.
fn worker_panel(
    view: &GlossaryView,
    palette: &theme::Palette,
    cx: &mut gpui_kit::Context<GlossaryView>,
) -> impl IntoElement {
    let mut body = div().flex().flex_col().flex_1().min_h_0().gap_3();
    if view.curator_loading && view.curator.is_none() {
        return body.child(
            div()
                .py_12()
                .text_center()
                .text_sm()
                .text_color(palette.muted_foreground)
                .child("Loading worker status…"),
        );
    }
    if let Some(err) = &view.curator_error {
        body = body.child(
            div()
                .text_sm()
                .text_color(palette.destructive)
                .child(err.clone()),
        );
    }
    let running = view.curator.as_ref().is_some_and(|s| s.running);
    let total = view.curator.as_ref().map(|s| s.terms_total).unwrap_or(0);
    let checked = view.curator.as_ref().map(|s| s.terms_checked).unwrap_or(0);
    let conflicts = view.curator.as_ref().map(|s| s.open_conflicts).unwrap_or(0);
    let dot = if running {
        palette.success
    } else {
        palette.muted_foreground
    };
    body = body.child(
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_4()
            .p_4()
            .rounded_md()
            .bg(palette.muted)
            .child(worker_stat(
                "Status",
                if running { "Running" } else { "Idle" },
                dot,
                palette,
            ))
            .child(worker_stat(
                "Terms checked",
                &format!("{checked} / {total}"),
                palette.primary,
                palette,
            ))
            .child(worker_stat(
                "Open conflicts",
                &format!("{conflicts}"),
                palette.destructive,
                palette,
            )),
    );
    let mut actions = div().flex().gap_2();
    if running {
        actions = actions.child(
            Button::new("curator-stop")
                .label("Stop after current term")
                .disabled(view.curator_loading)
                .on_click(cx.listener(|this, _, _, cx| this.curator_stop(cx))),
        );
    } else {
        actions = actions.child(
            Button::new("curator-start")
                .label("Start pass now")
                .primary()
                .disabled(view.curator_loading)
                .on_click(cx.listener(|this, _, _, cx| this.curator_start(cx))),
        );
    }
    actions = actions.child(
        Button::new("curator-refresh")
            .label("Refresh")
            .ghost()
            .disabled(view.curator_loading)
            .on_click(cx.listener(|this, _, _, cx| this.refresh_curator(cx))),
    );
    body = body.child(actions);
    if running {
        if let Some(term) = view
            .curator
            .as_ref()
            .and_then(|s| s.current_term.as_deref())
        {
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .rounded_md()
                    .bg(palette.accent_soft.opacity(0.4))
                    .text_sm()
                    .child(div().w(px(8.0)).h(px(8.0)).rounded_full().bg(palette.info))
                    .child(format!("Checking “{term}”…")),
            );
        }
    }
    body = body.child(div().text_sm().text_color(palette.muted_foreground).child(
        "The curator checks ~20 terms per pass against book passages and files \
             disagreements as conflicts. It also runs automatically every 5 minutes.",
    ));
    let passes = view
        .curator
        .as_ref()
        .map(|s| s.recent_passes.as_slice())
        .unwrap_or(&[]);
    if !passes.is_empty() {
        body = body.child(div().text_sm().font_medium().child("Recent passes"));
        let mut list = div().flex().flex_col().gap_1();
        for pass in passes {
            let ts = pass.get("ts").and_then(|v| v.as_str()).unwrap_or("?");
            let short_ts = ts.get(..19).unwrap_or(ts).replace('T', " ");
            let checked_n = pass.get("checked").and_then(|v| v.as_u64()).unwrap_or(0);
            let verified = pass.get("verified").and_then(|v| v.as_u64()).unwrap_or(0);
            let filed = pass
                .get("conflicts_filed")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_1p5()
                    .rounded_md()
                    .border_1()
                    .border_color(palette.border)
                    .text_sm()
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(palette.muted_foreground)
                            .child(short_ts),
                    )
                    .child(div().flex_1().child(format!(
                        "{checked_n} checked · {verified} verified · {filed} conflicts"
                    ))),
            );
        }
        body = body.child(list);
    }
    body
}

/// Conflicts review queue (red tab content): virtualized rows with keep /
/// accept actions. Empty → guidance text.
fn conflicts_panel(
    view: &GlossaryView,
    palette: &theme::Palette,
    cx: &mut gpui_kit::Context<GlossaryView>,
) -> impl IntoElement {
    let mut body = div().flex().flex_col().flex_1().min_h_0().gap_2();
    if view.conflicts.is_empty() {
        body = body.child(
            div()
                .py_12()
                .text_center()
                .text_sm()
                .text_color(palette.muted_foreground)
                .child("No conflicts — every suggestion is resolved."),
        );
        return body;
    }
    let entity = cx.entity();
    let state = view.conflicts_list.clone();
    let rows = list(state, move |ix, _window, cx: &mut App| {
        entity.update(cx, |this, cx| {
            GlossaryView::conflict_row(this, ix, theme::current_palette(), cx)
        })
    })
    .flex_1()
    .min_h_0();
    body.child(rows)
}

/// Muted pastel segment colors for the category breakdown (stable order,
/// Light-safe; mirrors the Model Usage Breakdown look).
fn category_colors(palette: &theme::Palette) -> [Hsla; 8] {
    [
        palette.success.opacity(0.75),
        palette.info.opacity(0.75),
        palette.warning.opacity(0.75),
        palette.primary.opacity(0.55),
        palette.destructive.opacity(0.55),
        palette.accent_foreground.opacity(0.6),
        palette.muted_foreground.opacity(0.6),
        palette.secondary_foreground.opacity(0.6),
    ]
}

/// Fixed stats footer: category breakdown bar over the FULL glossary
/// (independent of search/filters) — one muted pastel segment per type plus
/// uncategorized, with legend carrying count + percentage. Mirrors the
/// Model Usage Breakdown visualization. Real data only.
fn stats_footer(
    counts: &[(String, usize)],
    total_terms: usize,
    palette: &theme::Palette,
) -> impl IntoElement {
    let colors = category_colors(palette);
    let total = total_terms.max(1) as f64;
    let mut bar = div()
        .flex()
        .flex_row()
        .h(px(10.0))
        .rounded_sm()
        .bg(palette.muted)
        .overflow_hidden();
    for (i, (_, n)) in counts.iter().enumerate() {
        let frac = ((*n as f64 / total).clamp(0.0, 1.0)) as f32;
        if frac > 0.005 {
            bar = bar.child(
                div()
                    .h_full()
                    .bg(colors[i % colors.len()])
                    .w(relative(frac)),
            );
        }
    }
    let mut legend = div().flex().flex_row().flex_wrap().gap_x_3().gap_y_1();
    for (i, (label, n)) in counts.iter().enumerate() {
        let pct = *n as f64 / total * 100.0;
        legend = legend.child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .text_xs()
                .text_color(palette.muted_foreground)
                .child(
                    div()
                        .w(px(8.0))
                        .h(px(8.0))
                        .rounded_sm()
                        .bg(colors[i % colors.len()]),
                )
                .child(format!("{label} {n} · {pct:.0}%")),
        );
    }
    div()
        .flex()
        .flex_none()
        .flex_col()
        .gap_2()
        .pt_2()
        .border_t_1()
        .border_color(palette.border)
        .child(bar)
        .child(legend)
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
        this.apply_filter(cx);
    }))
}
