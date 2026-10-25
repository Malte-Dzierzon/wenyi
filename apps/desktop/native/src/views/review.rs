//! Review (screen 7): run history + run button + activity + issues.
//!
//! Parity source: `features/review/ReviewPage.tsx`, `ReviewActivity.tsx`,
//! `ReviewIssues.tsx`, `ReviewItemDetails.tsx`.
//! - History select (shown when >1 run); run button gated on
//!   `!historical && !busy && !active && translated && !loading && !error`.
//! - Activity panel: progress + elapsed + running state (poll-driven; WS later).
//! - Issues: client search + status filter, counts, expandable details with
//!   evidence/locations + proofreading deep-links (`?segment=` equivalent):
//!   the App pass sets `on_open_proofreading` so each issue link opens the
//!   chapter + segment in the Proofreading view.
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Disableable, StyledExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::{
    client::{block_on, is_project_busy, Client, ReviewRun},
    theme,
};

pub struct ReviewView {
    client: Option<Client>,
    pid: String,
    project_status: String,
    translated: bool,
    runs: Vec<ReviewRun>,
    selected_rid: Option<String>,
    run_detail: Option<ReviewRun>,
    workflow_status: String,
    workflow_active: bool,
    loading: bool,
    error: Option<String>,
    submitting: bool,
    search: Entity<InputState>,
    status_filter: String,
    expanded: std::collections::HashSet<String>,
    /// Deep-link callback: open (chapter, segment) in the Proofreading view.
    /// Set by the App pass (which owns both views); when `None` the row
    /// shows the target location as text only.
    on_open_proofreading: Option<std::rc::Rc<dyn Fn(u32, Option<u32>)>>,
}

impl ReviewView {
    pub fn new(pid: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search issues…"));
        Self {
            client: None,
            pid,
            project_status: String::new(),
            translated: false,
            runs: Vec::new(),
            selected_rid: None,
            run_detail: None,
            workflow_status: String::new(),
            workflow_active: false,
            loading: true,
            error: None,
            submitting: false,
            search,
            status_filter: String::new(),
            expanded: Default::default(),
            on_open_proofreading: None,
        }
    }

    /// Set the cross-screen link into Proofreading. The App pass wires this
    /// to open the chapter (and highlight the segment); without it the row
    /// shows the target location as text only.
    pub fn on_open_proofreading(mut self, f: impl Fn(u32, Option<u32>) + 'static) -> Self {
        self.on_open_proofreading = Some(std::rc::Rc::new(f));
        self
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
        let selected = self.selected_rid.clone();
        self.loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let runs = block_on(client.list_review_runs(&pid))?;
                    let project = block_on(client.get_project(&pid))?;
                    let chapters = block_on(client.list_chapters(&pid)).unwrap_or_default();
                    let workflow = block_on(client.get_workflow(&pid)).ok();
                    // Default selection: latest run (runs are desc by id).
                    let rid = selected.or_else(|| runs.first().map(|r| r.id.clone()));
                    let detail = match &rid {
                        Some(rid) => Some(block_on(client.get_review_run(&pid, rid))?),
                        None => None,
                    };
                    Ok::<_, crate::client::Error>((runs, project, chapters, workflow, rid, detail))
                })
                .await;
            _ = this.update(cx, |view: &mut ReviewView, cx| {
                view.loading = false;
                match result {
                    Ok((runs, project, chapters, workflow, rid, detail)) => {
                        view.runs = runs;
                        view.project_status = project.project.status;
                        view.translated = !chapters.is_empty()
                            && chapters.iter().all(|c| c.status == "done");
                        if let Some(workflow) = workflow {
                            view.workflow_active =
                                workflow.status == "running" || workflow.status == "queued";
                            view.workflow_status = workflow.status;
                        }
                        view.selected_rid = rid;
                        view.run_detail = detail;
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

    fn run_review(&mut self, cx: &mut Context<Self>) {
        let (Some(client), pid) = (self.client.clone(), self.pid.clone()) else {
            return;
        };
        self.submitting = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result =
                cx.background_spawn(async move { block_on(client.run_ai_review(&pid)) }).await;
            _ = this.update(cx, |view: &mut ReviewView, cx| {
                view.submitting = false;
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

    fn select_run(&mut self, rid: String, cx: &mut Context<Self>) {
        self.selected_rid = Some(rid);
        self.expanded.clear();
        self.refresh(cx);
    }

    fn toggle_expand(&mut self, id: String, cx: &mut Context<Self>) {
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
        cx.notify();
    }

    /// Run button gate — mirrors the source exactly.
    fn can_run(&self) -> bool {
        let historical = match (&self.selected_rid, self.runs.first()) {
            (Some(sel), Some(first)) => sel != &first.id,
            _ => false,
        };
        !historical
            && !self.busy()
            && !self.workflow_active
            && self.translated
            && !self.loading
            && self.error.is_none()
    }

    fn filtered_items(&self, cx: &App) -> Vec<crate::client::ReviewItem> {
        let Some(detail) = &self.run_detail else {
            return Vec::new();
        };
        let q = self.search.read(cx).value().to_string().to_lowercase();
        detail
            .items
            .iter()
            .filter(|item| {
                (self.status_filter.is_empty() || item.status == self.status_filter)
                    && (q.trim().is_empty()
                        || item.detail.to_lowercase().contains(&q)
                        || item.suggestion.to_lowercase().contains(&q))
            })
            .cloned()
            .collect()
    }
}

impl Render for ReviewView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current_palette();
        // Scroll region owns overflow (scrollbar overlay); flex_1 + min_h_0
        // lets it shrink inside the app-shell column.
        let mut col = div().flex().flex_col().flex_1().min_h_0().gap_3().p_6()
            .overflow_y_scrollbar();

        // Header: title + history select + run button.
        let mut header = div().flex().items_center().justify_between().gap_2().child(
            div().text_xl().font_weight(FontWeight::BOLD).child("Whole-book review"),
        );
        let mut actions = div().flex().items_center().gap_2();
        if self.runs.len() > 1 {
            let mut hist = div().flex().gap_1();
            for run in self.runs.clone() {
                let rid = run.id.clone();
                let active = self.selected_rid.as_deref() == Some(rid.as_str());
                let label = run.created_at.clone().unwrap_or_else(|| run.status.clone());
                let mut btn = Button::new(format!("run-{rid}")).label(label);
                if active {
                    btn = btn.primary();
                } else {
                    btn = btn.ghost();
                }
                hist = hist.child(btn.on_click(cx.listener(move |this, _, _, cx| {
                    this.select_run(rid.clone(), cx);
                })));
            }
            actions = actions.child(hist);
        }
        if self.can_run() {
            actions = actions.child(
                Button::new("run-review")
                    .primary()
                    .label(if self.submitting { "Submitting…" } else { "Run whole-book review" })
                    .disabled(self.submitting)
                    .on_click(cx.listener(|this, _, _, cx| this.run_review(cx))),
            );
        }
        header = header.child(actions);
        col = col.child(header);

        if let Some(err) = &self.error {
            col = col.child(
                div().p_3().rounded_md().bg(palette.destructive)
                    .text_color(palette.destructive_foreground)
                    .child(format!("Error: {err}")),
            );
        }

        if self.loading && self.run_detail.is_none() {
            col = col.child(div().text_sm().text_color(palette.muted_foreground).child("Loading…"));
            return col;
        }

        // Activity panel (poll-driven until WS lands).
        let running = self.workflow_active;
        col = col.child(
            div().flex().flex_col().gap_1().p_4().rounded_md()
                .border_1().border_color(palette.border).bg(palette.card)
                .child(div().text_sm().font_medium().child(format!(
                    "Workflow: {} · Review runs: {}",
                    if self.workflow_status.is_empty() { "—" } else { &self.workflow_status },
                    self.runs.len(),
                )))
                .child(Progress::new("review-activity").value(if running { 50.0 } else { 100.0 })),
        );

        let Some(detail) = self.run_detail.clone() else {
            col = col.child(
                div().py_12().text_center().text_sm().text_color(palette.muted_foreground)
                    .child("No review runs yet. Translate every chapter, then run a review."),
            );
            return col;
        };

        // Issues: search + status filter + counts + expandable details.
        col = col.child(
            div().flex().gap_2()
                .child(div().flex_1().child(Input::new(&self.search)))
                .child(
                    Button::new("issue-search")
                        .label("Search")
                        .on_click(cx.listener(|_, _, _, cx| cx.notify())),
                ),
        );
        let mut counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for item in &detail.items {
            *counts.entry(item.status.clone()).or_insert(0) += 1;
        }
        let mut filter_row = div().flex().gap_1();
        filter_row = filter_row.child(status_chip("", "All", &self.status_filter, cx));
        for status in counts.keys() {
            filter_row = filter_row.child(status_chip(status, status, &self.status_filter, cx));
        }
        col = col.child(filter_row);
        col = col.child(div().text_xs().text_color(palette.muted_foreground).child(format!(
            "{} issues · {}",
            detail.items.len(),
            counts.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join(", "),
        )));

        let items = self.filtered_items(cx);
        if items.is_empty() {
            col = col.child(
                div().text_sm().text_color(palette.muted_foreground).child("No matching issues."),
            );
        } else {
            let mut list = div().flex().flex_col().gap_1();
            for item in items {
                let expanded = self.expanded.contains(&item.id);
                let id = item.id.clone();
                let mut row = div().flex().flex_col().gap_1().p_3().rounded_md()
                    .border_1().border_color(palette.border)
                    .child(
                        div().flex().items_center().gap_2().text_sm()
                            .child(div().px_2().rounded_sm().bg(palette.muted).text_xs().child(item.status.clone()))
                            .child(div().font_medium().child(
                                if item.detail.is_empty() { item.kind.clone() } else { item.detail.clone() },
                            ))
                            .child(
                                Button::new(format!("expand-{}", id))
                                    .ghost()
                                    .label(if expanded { "Hide" } else { "Details" })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.toggle_expand(id.clone(), cx);
                                    })),
                            ),
                    );
                if expanded {
                    if !item.suggestion.is_empty() {
                        row = row.child(div().text_sm().child(format!("Suggestion: {}", item.suggestion)));
                    }
                    if let Some(loc) = &item.location {
                        let chapter = loc.chapter.max(0) as u32;
                        let segment = Some(loc.segment_index.max(0) as u32);
                        let mut loc_row = div().flex().items_center().gap_2()
                            .text_xs().text_color(palette.muted_foreground).child(format!(
                                "Ch.{} seg.{}: {}",
                                loc.chapter, loc.segment_index, loc.source,
                            ));
                        if let Some(cb) = self.on_open_proofreading.clone() {
                            loc_row = loc_row.child(
                                Button::new(format!("open-proof-{}", item.id))
                                    .label("Open in proofreading")
                                    .on_click(move |_, _, _| cb(chapter, segment)),
                            );
                        }
                        row = row.child(loc_row);
                    }
                    for ev in &item.evidence {
                        let chapter = ev.chapter.max(0) as u32;
                        let segment = Some(ev.segment_index.max(0) as u32);
                        let loc_text = format!("Evidence ch.{}: {}", ev.chapter, ev.source);
                        if let Some(cb) = self.on_open_proofreading.clone() {
                            row = row.child(
                                div().flex().items_center().gap_2()
                                    .text_xs().text_color(palette.muted_foreground).child(loc_text)
                                    .child(
                                        Button::new(format!("open-proof-ev-{}-{}", item.id, ev.segment_index))
                                            .label("Open")
                                            .on_click(move |_, _, _| cb(chapter, segment)),
                                    ),
                            );
                        } else {
                            row = row.child(div().text_xs().text_color(palette.muted_foreground).child(loc_text));
                        }
                    }
                }
                list = list.child(row);
            }
            col = col.child(list);
        }
        col
    }
}

fn status_chip(
    status: &str,
    label: &str,
    active: &str,
    cx: &mut gpui_kit::Context<ReviewView>,
) -> impl IntoElement {
    let status = status.to_string();
    let mut btn = Button::new(format!("status-{status}")).label(label.to_string());
    if active == status {
        btn = btn.primary();
    } else {
        btn = btn.ghost();
    }
    btn.on_click(cx.listener(move |this: &mut ReviewView, _, _, cx| {
        this.status_filter = status.clone();
        cx.notify();
    }))
}
