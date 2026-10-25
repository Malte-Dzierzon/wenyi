//! Progress / overview (screen 3): run controls + overview + chapters.
//!
//! Parity source: `features/progress/ProgressPage.tsx` (+ WorkflowPanel,
//! Accounting, UsageChart, RunTimeChart, accountingData.ts, useLiveTiming).
//! Header actions (translate/pause/resume with exact visibility conditions),
//! overview card (done/total + Progress), workflow stages list, connection
//! label, not-initialized note, report disclosure + regenerate, ChapterTable
//! (5 columns + per-chapter translate + review badges), and the Accounting
//! block: metric cards + usage breakdown (group tabs + stacked token bars)
//! + run history (kit `BarChart` + duration rows) + usage-over-time
//! (kit `LineChart`). Polling replaces WS for now (WS client lands with the
//! progress bus); one 3s while-busy round-trip covers the source's 2.5s
//! project / 3s chapters / 5s stats+report cadences.
//! Read-contents link (`on_open_contents`, App-wired → Contents screen)
//! mirrors the React contents link for book projects with chapters.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Disableable, StyledExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use super::widgets::{
    self, UsageGroup, UsageMetrics,
};
use crate::{
    client::{
        block_on, is_project_busy, ChapterSummary, Client, Project, ProjectStats, WorkflowOut,
    },
    theme,
};

/// Load all progress data in one background round-trip.
/// Report fetch failures are non-fatal (mirrors the disclosure-scoped
/// `ErrorNotice`): stored separately so the page still renders.
struct Snapshot {
    project: Project,
    project_total_words: u64,
    chapters: Vec<ChapterSummary>,
    stats: Option<ProjectStats>,
    stats_running: bool,
    workflow: Option<WorkflowOut>,
    report: Option<serde_json::Value>,
    report_error: Option<String>,
}

fn load(client: &Client, pid: &str) -> Result<Snapshot, crate::client::Error> {
    let detail = block_on(client.get_project(pid))?;
    let project = detail.project;
    let project_total_words = detail.total_word_count;
    let stats_running = is_project_busy(&project.status);
    let chapters = block_on(client.list_chapters(pid)).unwrap_or_default();
    let stats = block_on(client.get_stats(pid)).ok();
    let workflow = block_on(client.get_workflow(pid)).ok();
    let (report, report_error) = match block_on(client.get_report(pid)) {
        Ok(report) => (Some(report), None),
        Err(err) => (None, Some(err.to_string())),
    };
    Ok(Snapshot {
        project,
        project_total_words,
        chapters,
        stats,
        stats_running,
        workflow,
        report,
        report_error,
    })
}

pub struct ProgressView {
    client: Option<Client>,
    pid: String,
    snapshot: Option<Snapshot>,
    error: Option<String>,
    action_pending: bool,
    regen_pending: bool,
    translating_chapter: Option<u32>,
    usage_group: UsageGroup,
    /// Cross-screen link into Contents (mirrors the React
    /// "read contents source" link), set by the App pass.
    on_open_contents: Option<std::rc::Rc<dyn Fn()>>,
}

impl ProgressView {
    pub fn new(pid: String) -> Self {
        Self {
            client: None,
            pid,
            snapshot: None,
            error: None,
            action_pending: false,
            regen_pending: false,
            translating_chapter: None,
            usage_group: UsageGroup::ByStage,
            on_open_contents: None,
        }
    }

    /// Set the cross-screen link into Contents. Without it the overview
    /// shows no read-contents affordance.
    pub fn on_open_contents(mut self, f: impl Fn() + 'static) -> Self {
        self.on_open_contents = Some(std::rc::Rc::new(f));
        self
    }

    pub fn set_client(&mut self, client: Client, cx: &mut Context<Self>) {
        self.client = Some(client);
        self.refresh(cx);
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let (Some(client), pid) = (self.client.clone(), self.pid.clone()) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result =
                cx.background_spawn(async move { load(&client, &pid) }).await;
            _ = this.update(cx, |view: &mut ProgressView, cx| {
                match result {
                    Ok(snapshot) => {
                        view.snapshot = Some(snapshot);
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
        // Poll while busy (WS lands later); 3s cadence like chapter queries.
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(3))
                .await;
            _ = this.update(cx, |view: &mut ProgressView, cx| {
                let busy = view
                    .snapshot
                    .as_ref()
                    .map(|s| is_project_busy(&s.project.status))
                    .unwrap_or(true);
                if busy {
                    view.refresh(cx);
                }
            });
        })
        .detach();
    }

    fn run_action(&mut self, kind: &str, cx: &mut Context<Self>) {
        let (Some(client), pid) = (self.client.clone(), self.pid.clone()) else {
            return;
        };
        let kind = kind.to_string();
        self.action_pending = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    match kind.as_str() {
                        "pause" => block_on(client.pause(&pid)),
                        "resume" => block_on(client.resume(&pid)),
                        _ => block_on(client.translate(&pid)),
                    }
                })
                .await;
            _ = this.update(cx, |view: &mut ProgressView, cx| {
                view.action_pending = false;
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

    fn regenerate(&mut self, cx: &mut Context<Self>) {
        let (Some(client), pid) = (self.client.clone(), self.pid.clone()) else {
            return;
        };
        self.regen_pending = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result =
                cx.background_spawn(async move { block_on(client.regenerate_report(&pid)) }).await;
            _ = this.update(cx, |view: &mut ProgressView, cx| {
                view.regen_pending = false;
                match result {
                    Ok(report) => {
                        if let Some(s) = view.snapshot.as_mut() {
                            s.report = Some(report);
                            s.report_error = None;
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

    fn translate_chapter(&mut self, ci: u32, cx: &mut Context<Self>) {
        let (Some(client), pid) = (self.client.clone(), self.pid.clone()) else {
            return;
        };
        self.translating_chapter = Some(ci);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { block_on(client.translate_chapter(&pid, ci)) })
                .await;
            _ = this.update(cx, |view: &mut ProgressView, cx| {
                view.translating_chapter = None;
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

    /// Review badge text — mirrors ChapterTable logic: issue count +
    /// `review_status ∈ {completed,ok,done}`.
    fn review_badge(ch: &ChapterSummary) -> String {
        if ch.review_issue_count > 0 {
            return format!("{} issues", ch.review_issue_count);
        }
        match ch.review_status.as_str() {
            "completed" | "ok" | "done" => "reviewed".into(),
            _ => "not reviewed".into(),
        }
    }
}

impl Render for ProgressView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current_palette();
        // Scroll region owns overflow (scrollbar overlay); flex_1 + min_h_0
        // lets it shrink inside the app-shell column.
        let mut col = div().flex().flex_col().flex_1().min_h_0().gap_3().p_6()
            .overflow_y_scrollbar();

        let Some(snapshot) = &self.snapshot else {
            col = col.child(div().text_xl().font_weight(FontWeight::BOLD).child("Translation overview"));
            if let Some(err) = &self.error {
                col = col.child(error_box(err, &palette));
            } else {
                col = col.child(
                    div().text_sm().text_color(palette.muted_foreground).child("Loading…"),
                );
            }
            return col;
        };

        let project = &snapshot.project;
        let busy = is_project_busy(&project.status);
        let paused = project.status == "paused";
        // Segment counts, not finished chapters (mirrors source comment).
        let done: u64 = snapshot.chapters.iter().map(|c| c.target_word_count).sum();
        let total: u64 = snapshot
            .chapters
            .iter()
            .map(|c| c.word_count)
            .sum::<u64>()
            .max(snapshot.project_total_words);
        let pct = if total > 0 { ((done as f64 / total as f64) * 100.0).round() as f32 } else { 0.0 };

        // Header with exact visibility conditions from ProgressPage.
        let mut header = div().flex().items_center().justify_between().gap_2().child(
            div().flex().flex_col().gap_1().child(
                div().text_xl().font_weight(FontWeight::BOLD).child(project.name.clone()),
            ).child(
                div().text_sm().text_color(palette.muted_foreground).child(format!(
                    "{} → {} · {}",
                    project.source_lang.as_deref().unwrap_or("auto"),
                    project.target_lang.as_deref().unwrap_or("—"),
                    project.title.as_deref().unwrap_or("waiting for source"),
                )),
            ),
        );
        let mut actions = div().flex().items_center().gap_2();
        if !busy && !paused && project.status != "error" && project.fmt.is_some() {
            actions = actions.child(
                Button::new("start-translation")
                    .primary()
                    .label("Start translation")
                    .disabled(self.action_pending)
                    .on_click(cx.listener(|this, _, _, cx| this.run_action("translate", cx))),
            );
        }
        if busy && project.status != "parsing" {
            actions = actions.child(
                Button::new("pause")
                    .label("Pause")
                    .disabled(self.action_pending || project.status == "pausing")
                    .on_click(cx.listener(|this, _, _, cx| this.run_action("pause", cx))),
            );
        }
        if paused || project.status == "error" {
            actions = actions.child(
                Button::new("resume")
                    .label("Resume task")
                    .disabled(self.action_pending)
                    .on_click(cx.listener(|this, _, _, cx| this.run_action("resume", cx))),
            );
        }
        header = header.child(actions);
        col = col.child(header);

        if let Some(err) = &self.error {
            col = col.child(error_box(err, &palette));
        }
        if let Some(err) = &project.error {
            col = col.child(error_box(err, &palette));
        }

        // Overview card: done/total + progress bar + Accounting block
        // (mirrors the `p-5 space-y-4` Card in ProgressPage).
        let mut overview = div()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_md()
            .border_1()
            .border_color(palette.border)
            .bg(palette.card)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().font_weight(FontWeight::MEDIUM).child("Translation overview"))
                    .child(
                        div()
                            .px_2()
                            .rounded_sm()
                            .bg(palette.muted)
                            .text_xs()
                            .child(project.status.clone()),
                    ),
            )
            .child(div().text_sm().child(format!("{done} / {total}")))
            .child(Progress::new("overview-progress").value(pct));

        // Stats: metric cards + usage breakdown + run history + usage chart.
        // Mirrors `Accounting` (value + running props from the stats query).
        match &snapshot.stats {
            Some(stats) => {
                let metrics = UsageMetrics::from_stats(stats, snapshot.stats_running);
                overview = overview.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Total usage & run time"))
                        .child(widgets::metric_cards(&metrics, &palette)),
                );
                let group = self.usage_group;
                let mut tabs = div().flex().flex_row().gap_1().p_1().rounded_md().bg(palette.muted);
                for g in UsageGroup::ALL {
                    let active = g == group;
                    let mut btn = Button::new(format!("usage-group-{}", g.key())).label(g.tab_label());
                    if active {
                        btn = btn.primary();
                    } else {
                        btn = btn.ghost();
                    }
                    tabs = tabs.child(btn.on_click(cx.listener(move |this, _, _, cx| {
                        this.usage_group = g;
                        cx.notify();
                    })));
                }
                overview = overview.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(tabs)
                        .child(widgets::usage_rows(&stats.usage, group, &palette)),
                );
                let runs = widgets::timing_runs(&stats.timing);
                overview = overview.child(widgets::run_chart(&runs, &palette));
                overview = overview.child(widgets::usage_over_time_chart(&runs, &palette));
            }
            None => {
                overview = overview.child(
                    div().text_sm().text_color(palette.muted_foreground).child("No usage recorded yet"),
                );
            }
        }
        col = col.child(overview);

        // Workflow panel (mirrors WorkflowPanel stage list; live label via poll).
        if let Some(workflow) = &snapshot.workflow {
            let mut stages = div().flex().flex_col().gap_1();
            for (i, stage) in workflow.stages.iter().enumerate() {
                let label = if stage.enabled {
                    format!("{} · {} — enabled", i + 1, stage.label)
                } else {
                    format!("{} · {} — disabled", i + 1, stage.label)
                };
                stages = stages.child(
                    div()
                        .text_sm()
                        .text_color(if stage.enabled { palette.foreground } else { palette.muted_foreground })
                        .child(format!("{} {}", if stage.enabled { "●" } else { "○" }, label)),
                );
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
                    .bg(palette.card)
                    .child(div().font_weight(FontWeight::MEDIUM).child("Current workflow"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(workflow.kind.clone()))
                            .child(
                                div()
                                    .px_2()
                                    .rounded_sm()
                                    .bg(palette.muted)
                                    .text_xs()
                                    .child(workflow.status.clone()),
                            ),
                    )
                    .child(stages),
            );
        }

        // Connection label (polling until WS lands).
        col = col.child(
            div().text_xs().text_color(palette.muted_foreground).child("Progress: polling"),
        );

        // Read-contents link (book projects with chapters; mirrors React).
        let subtitle = project.fmt.as_deref() == Some("srt");
        if !subtitle && !snapshot.chapters.is_empty() {
            if let Some(cb) = self.on_open_contents.clone() {
                col = col.child(
                    Button::new("read-contents")
                        .label("Read contents source")
                        .on_click(move |_, _, _| cb()),
                );
            }
        }

        // Chapter table: 5 columns (chapter, source paragraphs→words, status,
        // review, actions). Mirrors ChapterTable.
        let mut table = div().flex().flex_col().gap_1();
        table = table.child(
            div()
                .flex()
                .gap_2()
                .text_xs()
                .font_weight(FontWeight::MEDIUM)
                .text_color(palette.muted_foreground)
                .child(div().w_64().child("Chapter"))
                .child(div().flex_1().child("Title"))
                .child(div().w_24().child("Words"))
                .child(div().w_24().child("Status"))
                .child(div().w_32().child("Review"))
                .child(div().w_32().child("")),
        );
        if snapshot.chapters.is_empty() {
            table = table.child(
                div().text_sm().text_color(palette.muted_foreground).child("No chapters yet."),
            );
        }
        for ch in &snapshot.chapters {
            let ci = ch.index;
            let translating = self.translating_chapter == Some(ci);
            table = table.child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .text_sm()
                    .child(div().w_64().child(format!("#{}", ch.index)))
                    .child(div().flex_1().child(ch.title.clone()))
                    .child(div().w_24().child(format!("{}/{}", ch.target_word_count, ch.word_count)))
                    .child(div().w_24().child(ch.status.clone()))
                    .child(div().w_32().text_xs().child(Self::review_badge(ch)))
                    .child(
                        Button::new(format!("translate-ch-{ci}"))
                            .label(if translating { "…" } else { "Translate" })
                            .disabled(busy || translating)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.translate_chapter(ci, cx);
                            })),
                    ),
            );
        }
        col = col.child(table);

        // Report disclosure + regenerate.
        if !project.initialized {
            col = col.child(
                div().text_sm().text_color(palette.muted_foreground)
                    .child("Upload a source file to initialize this project."),
            );
        }
        col = col.child(
            div().flex().items_center().gap_2().child(
                Button::new("regen-report")
                    .label("Update report")
                    .disabled(self.regen_pending || busy || !project.initialized)
                    .on_click(cx.listener(|this, _, _, cx| this.regenerate(cx))),
            ),
        );
        if let Some(report) = &snapshot.report {
            // Mirror the disclosure body: summary minus usage/timing ledgers
            // (those render as charts above).
            col = col.child(
                div().text_xs().text_color(palette.muted_foreground).child(report_summary(report)),
            );
        } else if let Some(err) = &snapshot.report_error {
            // Mirrors the disclosure-scoped ErrorNotice (e.g. 500 before first
            // translation produces a report).
            col = col.child(
                div().text_xs().text_color(palette.muted_foreground).child(format!(
                    "No report yet ({err})"
                )),
            );
        }
        col
    }
}

fn error_box(err: &str, palette: &theme::Palette) -> impl IntoElement {
    div()
        .p_3()
        .rounded_md()
        .bg(palette.destructive)
        .text_color(palette.destructive_foreground)
        .child(format!("Error: {err}"))
}

/// Report summary minus the usage/timing ledgers (those render as charts
/// above). Mirrors the `StructuredData` filter in ProgressPage.
fn report_summary(report: &serde_json::Value) -> String {
    match report.get("summary") {
        Some(serde_json::Value::Object(map)) if !map.is_empty() => map
            .iter()
            .filter(|(k, _)| *k != "usage" && *k != "timing")
            .take(8)
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Vec<_>>()
            .join(", "),
        Some(_) | None => compact_json(report),
    }
}

/// Compact one-line JSON summary (usage/timing ledgers, report).
fn compact_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) if !map.is_empty() => map
            .iter()
            .take(6)
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Vec<_>>()
            .join(", "),
        serde_json::Value::Null => "—".into(),
        other => other.to_string().chars().take(200).collect(),
    }
}
