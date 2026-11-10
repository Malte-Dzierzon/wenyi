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

use gpui_kit::component::accordion::Accordion;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::link::Link;
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tab::{Tab, TabBar};
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
#[derive(PartialEq)]
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
    // Independent reads run concurrently (one runtime hop); wall time is
    // the slowest call (~600ms), not the sum (~1.4s).
    let (detail, chapters, stats, workflow, report) = block_on(async {
        tokio::join!(
            client.get_project(pid),
            client.list_chapters(pid),
            client.get_stats(pid),
            client.get_workflow(pid),
            client.get_report(pid),
        )
    });
    let detail = detail?;
    let project = detail.project;
    let project_total_words = detail.total_word_count;
    let stats_running = is_project_busy(&project.status);
    let chapters = chapters.unwrap_or_default();
    let stats = stats.ok();
    let workflow = workflow.ok();
    let (report, report_error) = match report {
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
    /// Whether this screen is currently shown; the 3s busy-poll timer only
    /// re-arms while visible (set by the App shell on navigation).
    visible: bool,
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
            visible: true,
        }
    }

    /// Called by the App shell on navigation. Becoming visible resumes the
    /// poll loop with an immediate refresh.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if visible && !self.visible {
            self.visible = true;
            self.refresh(cx);
        } else {
            self.visible = visible;
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
                        if view.snapshot.as_ref() != Some(&snapshot) || view.error.is_some() {
                            view.snapshot = Some(snapshot);
                            view.error = None;
                            cx.notify();
                        }
                    }
                    Err(err) => {
                        let msg = err.to_string();
                        if view.error.as_ref() != Some(&msg) {
                            view.error = Some(msg);
                            cx.notify();
                        }
                    }
                }
            });
        })
        .detach();
        // Poll while busy (WS lands later); 3s cadence like chapter queries.
        // Only re-arms while visible; navigation resumes via `set_visible`.
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(3))
                .await;
            _ = this.update(cx, |view: &mut ProgressView, cx| {
                if !view.visible {
                    return;
                }
                // Persistent load error with no snapshot: stop polling.
                // Hammering a failing endpoint every 3s helps nobody;
                // revisiting the screen retries via set_visible.
                if view.error.is_some() && view.snapshot.is_none() {
                    return;
                }
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

    /// Chapters panel for the stats-grid side column: compact rows (title +
    /// words + status pill + per-chapter translate) in a fixed-height scroll
    /// container, so long chapter lists never extend the page. The full
    /// 6-column table doesn't fit half width — rows stack like the
    /// proofreading chapter list. The translate button only shows for
    /// chapters that are not done (mirrors the source condition).
    fn chapter_panel(
        &self,
        chapters: &[ChapterSummary],
        busy: bool,
        palette: &theme::Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut list = div().flex().flex_col();
        if chapters.is_empty() {
            list = list.child(
                div()
                    .p_8()
                    .text_center()
                    .text_sm()
                    .text_color(palette.muted_foreground)
                    .child("No chapters yet."),
            );
        }
        let last = chapters.len().saturating_sub(1);
        for (i, ch) in chapters.iter().enumerate() {
            let ci = ch.index;
            let translating = self.translating_chapter == Some(ci);
            let mut row = div()
                .flex()
                .items_center()
                .gap_2()
                .py_2()
                .text_sm()
                .when(i != last, |this| {
                    this.border_b_1().border_color(palette.border)
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .truncate()
                                .font_weight(FontWeight::MEDIUM)
                                .child(format!("#{} {}", ch.index, ch.title)),
                        )
                        .child(
                            div().text_xs().text_color(palette.muted_foreground).child(format!(
                                "{}/{} words · {}",
                                ch.target_word_count,
                                ch.word_count,
                                Self::review_badge(ch),
                            )),
                        ),
                )
                .child(
                    div()
                        .px_2()
                        .rounded_sm()
                        .bg(palette.muted)
                        .text_xs()
                        .flex_none()
                        .child(ch.status.clone()),
                );
            if ch.status != "done" {
                row = row.child(
                    Button::new(format!("translate-ch-{ci}"))
                        .label(if translating { "…" } else { "Translate" })
                        .disabled(busy || translating)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.translate_chapter(ci, cx);
                        })),
                );
            }
            list = list.child(row);
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Chapters"))
            .child(div().h(px(320.0)).overflow_y_scrollbar().child(list))
    }

    /// Workflow section for the stats-grid side panel: kind + status header,
    /// then one row per stage (number square + label + state pill). No card
    /// frame — the side panel provides the fill.
    fn workflow_section(workflow: Option<&WorkflowOut>, palette: &theme::Palette) -> impl IntoElement {
        let Some(workflow) = workflow else {
            return div()
                .text_sm()
                .text_color(palette.muted_foreground)
                .child("No workflow yet")
                .into_any_element();
        };
        let last_stage = workflow.stages.len().saturating_sub(1);
        let mut stages = div().flex().flex_col();
        for (i, stage) in workflow.stages.iter().enumerate() {
            stages = stages.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .py_2()
                    .text_sm()
                    .when(i != last_stage, |this| {
                        this.border_b_1().border_color(palette.border)
                    })
                    .child(
                        div()
                            .w(px(22.0))
                            .h(px(22.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_sm()
                            .bg(palette.muted)
                            .text_xs()
                            .text_color(palette.muted_foreground)
                            .flex_none()
                            .child(format!("{}", i + 1)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(if stage.enabled {
                                palette.foreground
                            } else {
                                palette.muted_foreground
                            })
                            .child(stage.label.clone()),
                    )
                    .child(
                        div()
                            .px_2()
                            .rounded_sm()
                            .text_xs()
                            .flex_none()
                            .when(stage.enabled, |this| {
                                this.bg(palette.success.opacity(0.15)).text_color(palette.success)
                            })
                            .when(!stage.enabled, |this| {
                                this.bg(palette.muted).text_color(palette.muted_foreground)
                            })
                            .child(if stage.enabled { "on" } else { "off" }),
                    ),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Current workflow"))
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
                    ),
            )
            .child(div().h(px(160.0)).overflow_y_scrollbar().child(stages))
            .into_any_element()
    }
}

impl Render for ProgressView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current_palette();
        // Scroll region owns overflow (scrollbar overlay); flex_1 + min_h_0
        // lets it shrink inside the app-shell column.
        let mut col = div().flex().flex_col().flex_1().min_h_0().gap_3().p_6()
            .overflow_y_scrollbar().font(theme::fonts::ui());

        let Some(snapshot) = &self.snapshot else {
            col = col.child(super::ui::page_title("Translation overview", None::<Div>));
            if let Some(err) = &self.error {
                col = col.child(super::ui::error_box(err, &palette));
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
        let caption = format!(
            "{} → {} · {}",
            project.source_lang.as_deref().unwrap_or("auto"),
            project.target_lang.as_deref().unwrap_or("—"),
            project.title.as_deref().unwrap_or("waiting for source"),
        );
        let mut header = div().flex().items_center().justify_between().gap_2().child(
            super::ui::page_title(project.name.clone(), Some(caption)),
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
            col = col.child(super::ui::error_box(err, &palette));
        }
        if let Some(err) = &project.error {
            col = col.child(super::ui::error_box(err, &palette));
        }

        // The page itself is the dashboard canvas. Only data groups get a
        // surface, avoiding one oversized card around every insight.
        let mut overview = div()
            .flex()
            .flex_col()
            .gap_5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(div().font_weight(FontWeight::MEDIUM).child("Translation overview"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            // Secondary connection state, de-emphasized next
                            // to the status (was a standalone row).
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(palette.muted_foreground)
                                    .child("live · polling"),
                            )
                            .child(
                                div()
                                    .px_2()
                                    .rounded_sm()
                                    .bg(palette.muted)
                                    .text_xs()
                                    .child(project.status.clone()),
                            ),
                    ),
            )
            .child(div().text_sm().child(format!("{done} / {total}")))
            .child(Progress::new("overview-progress").value(pct));

        // Stats grid: metric row on top, then a two-column grid — usage
        // visualization left, run history + workflow right — and a compact
        // over-time strip below. Sections use spacing + muted fills instead
        // of boxed cards (angular direction, no border stacks).
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
                let selected = UsageGroup::ALL.iter().position(|g| *g == group).unwrap_or(0);
                let tabs = TabBar::new("usage-group-tabs")
                    .segmented()
                    .selected_index(selected)
                    .on_click(cx.listener(|this, index: &usize, _, cx| {
                        if let Some(g) = UsageGroup::ALL.get(*index) {
                            this.usage_group = *g;
                            cx.notify();
                        }
                    }))
                    .child(Tab::new().label(UsageGroup::ByStage.tab_label()))
                    .child(Tab::new().label(UsageGroup::ByModel.tab_label()))
                    .child(Tab::new().label(UsageGroup::ByProvider.tab_label()));
                let usage_viz = match group {
                    UsageGroup::ByStage => {
                        widgets::usage_rows(&stats.usage, group, &palette).into_any_element()
                    }
                    UsageGroup::ByModel => {
                        widgets::usage_donut(&stats.usage, &palette).into_any_element()
                    }
                    UsageGroup::ByProvider => {
                        widgets::usage_provider_donut(&stats.usage, &palette).into_any_element()
                    }
                };
                let runs = widgets::timing_runs(&stats.timing);
                let usage_panel = div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_4()
                    .rounded_sm()
                    .bg(palette.muted.opacity(0.35))
                    .child(tabs)
                    // Center the visualization in the leftover panel height
                    // (the panel stretches to the side panel; tall content
                    // like stage bars is unaffected).
                    .child(div().flex_1().flex().flex_col().justify_center().child(usage_viz));
                let side_panel = div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .p_4()
                    .rounded_sm()
                    .bg(palette.muted.opacity(0.35))
                    .child(widgets::run_chart(&runs, &palette))
                    .child(self.chapter_panel(&snapshot.chapters, busy, &palette, cx));
                overview = overview.child(
                    div().flex().gap_3().child(usage_panel).child(side_panel),
                );
                overview = overview.child(widgets::usage_over_time_compact(&runs, &palette));
            }
            None => {
                overview = overview.child(
                    div().text_sm().text_color(palette.muted_foreground).child("No usage recorded yet"),
                );
            }
        }
        col = col.child(overview);

        // Read-contents link (book projects with chapters; mirrors React).
        let subtitle = project.fmt.as_deref() == Some("srt");
        if !subtitle && !snapshot.chapters.is_empty() {
            if let Some(cb) = self.on_open_contents.clone() {
                col = col.child(
                    Link::new("read-contents")
                        .child("Read contents source")
                        .on_click(move |_, _, _| cb()),
                );
            }
        }

        // Project report disclosure (mirrors the React `Disclosure`): update
        // button + structured summary, placed before the chapter table like
        // the source.
        let mut report_body = div().flex().flex_col().gap_2().child(
            Button::new("regen-report")
                .label("Update report")
                .disabled(self.regen_pending || busy || !project.initialized)
                .on_click(cx.listener(|this, _, _, cx| this.regenerate(cx))),
        );
        if let Some(report) = &snapshot.report {
            // Mirror the disclosure body: summary minus usage/timing ledgers
            // (those render as charts above), one row per key.
            for (key, text) in report_rows(report) {
                report_body = report_body.child(
                    div()
                        .flex()
                        .justify_between()
                        .gap_4()
                        .text_xs()
                        .child(div().text_color(palette.muted_foreground).child(key))
                        .child(div().flex_1().text_right().child(text)),
                );
            }
        } else if let Some(err) = &snapshot.report_error {
            // Mirrors the disclosure-scoped ErrorNotice (e.g. 500 before first
            // translation produces a report).
            report_body = report_body.child(
                div().text_xs().text_color(palette.muted_foreground).child(format!(
                    "No report yet ({err})"
                )),
            );
        }
        col = col.child(
            Accordion::new("project-report").item(|item| {
                item.title("Project report").child(report_body)
            }),
        );

        // Not-initialized note. The source links to the create screen with a
        // project prefill the native create screen has no API for, so this
        // stays a plain note rather than a dead link.
        if !project.initialized {
            col = col.child(
                div().text_sm().text_color(palette.muted_foreground)
                    .child("Upload a source file to initialize this project."),
            );
        }

        // Current workflow at the bottom (full width): kind + status header
        // plus the stage list, in a muted panel matching the grid above.
        col = col.child(
            div()
                .flex()
                .flex_col()
                .p_4()
                .rounded_sm()
                .bg(palette.muted.opacity(0.35))
                .child(Self::workflow_section(snapshot.workflow.as_ref(), &palette)),
        );

        // Bottom spacer so the last section scrolls fully into view
        // instead of clipping at the container edge.
        col = col.child(div().h(px(24.0)).flex_none());
        col
    }
}


/// Report summary minus the usage/timing ledgers (those render as charts
/// above), as key/value rows. Mirrors the `StructuredData` filter.
fn report_rows(report: &serde_json::Value) -> Vec<(String, String)> {
    let map = report
        .get("summary")
        .and_then(|v| v.as_object())
        .filter(|m| !m.is_empty())
        .map(|m| m.iter().collect::<Vec<_>>());
    let entries = map.unwrap_or_else(|| {
        report.as_object().map(|m| m.iter().collect()).unwrap_or_default()
    });
    entries
        .into_iter()
        .filter(|(k, _)| *k != "usage" && *k != "timing")
        .take(8)
        .map(|(k, v)| {
            let text = match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Null => "—".into(),
                other => {
                    let s = other.to_string();
                    if s.len() > 120 { format!("{}…", &s[..120]) } else { s }
                }
            };
            (k.clone(), text)
        })
        .collect()
}
