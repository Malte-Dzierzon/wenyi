//! Progress / overview (screen 3): run controls + overview + chapters.
//!
//! Parity source: `features/progress/ProgressPage.tsx` (+ WorkflowPanel).
//! Header actions (translate/pause/resume with exact visibility conditions),
//! overview card (done/total + Progress), workflow stages list, connection
//! label, not-initialized note, ChapterTable (5 columns + per-chapter
//! translate + review badges). Polling replaces WS for now (WS lands
//! with the progress bus); one 3s while-busy round-trip covers the source's
//! 2.5s project / 3s chapters cadences. Stats/report cards were removed;
//! run usage + history live behind the engine service.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::Disableable;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::{
    service::{
        is_project_busy, ChapterSummary, Project, ReviewRunOut, ServiceError, ServiceHandle,
        WorkflowOut,
    },
    theme,
};

/// Load all progress data in one background round-trip.
#[derive(PartialEq)]
struct Snapshot {
    project: Project,
    project_total_words: u64,
    chapters: Vec<ChapterSummary>,
    workflow: Option<WorkflowOut>,
    usage: serde_json::Value,
    timing: serde_json::Value,
    review_runs: Vec<ReviewRunOut>,
    /// Hash of the `usage`/`timing` ledgers; the view memoizes derived
    /// visualization inputs on it (see `LedgerViz`).
    ledger_hash: u64,
}

/// Hash the usage/timing ledgers for change detection. `serde_json::Value`
/// has no `Hash` impl; the canonical string form is stable for equal
/// ledgers and cheap at ledger sizes (a few KB).
fn ledger_hash(usage: &serde_json::Value, timing: &serde_json::Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    usage.to_string().hash(&mut hasher);
    timing.to_string().hash(&mut hasher);
    hasher.finish()
}

/// Ledger-derived display inputs (metrics + run rows). Rebuilt only when
/// the ledger hash changes — GPUI elements rebuild every render by design,
/// so this memoizes the JSON walks for unrelated renders (translate
/// spinner, tab switch, window events).
#[derive(Clone)]
struct LedgerViz {
    hash: u64,
    metrics: super::widgets::UsageMetrics,
    runs: Vec<super::widgets::RunRow>,
}

fn load(service: &ServiceHandle, pid: &str) -> Result<Snapshot, ServiceError> {
    let started = std::time::Instant::now();
    // Parallelize the five independent reads in ONE background task via
    // scoped threads (no new deps; mirrors the old `tokio::join!`): wall
    // time is the slowest leg, not the sum. All five legs share the 5s
    // cross-view cache, so steady-state refreshes are usually free.
    let (detail, chapters, workflow, stats, review_runs) = std::thread::scope(|s| {
        let detail = s.spawn(|| service.get_project(pid));
        let chapters = s.spawn(|| service.list_chapters(pid));
        let workflow = s.spawn(|| service.get_workflow(pid));
        let stats = s.spawn(|| service.get_stats(pid));
        let review_runs = s.spawn(|| service.list_review_runs(pid));
        (
            detail.join(),
            chapters.join(),
            workflow.join(),
            stats.join(),
            review_runs.join(),
        )
    });
    let detail = match detail {
        Ok(result) => result?,
        Err(_) => return Err(ServiceError::Engine("project load failed".into())),
    };
    let project = detail.project;
    let project_total_words = detail.total_word_count;
    let chapters = chapters.ok().and_then(|r| r.ok()).unwrap_or_default();
    let workflow = workflow.ok().and_then(|r| r.ok());
    let stats = stats.ok().and_then(|r| r.ok()).unwrap_or_default();
    let review_runs = review_runs.ok().and_then(|r| r.ok()).unwrap_or_default();
    tracing::debug!(
        elapsed_ms = started.elapsed().as_millis(),
        "progress snapshot loaded"
    );
    let usage = stats.usage;
    let timing = stats.timing;
    let hash = ledger_hash(&usage, &timing);
    Ok(Snapshot {
        project,
        project_total_words,
        chapters,
        workflow,
        usage,
        timing,
        review_runs,
        ledger_hash: hash,
    })
}

pub struct ProgressView {
    service: Option<ServiceHandle>,
    pid: String,
    snapshot: Option<Snapshot>,
    error: Option<String>,
    action_pending: bool,
    /// Chapter with a translate in flight (row spinner); cleared on completion.
    translating_chapter: Option<u32>,
    /// Whether this screen is currently shown; the 3s busy-poll timer only
    /// re-arms while visible (set by the App shell on navigation).
    visible: bool,
    /// Usage breakdown grouping (stage/model/provider tabs).
    usage_group: super::widgets::UsageGroup,
    /// Singularized busy-poll timer: at most one timer task is armed at a
    /// time (the old code spawned one per refresh — O(n) timers after n
    /// polls). `poll_token` guards the chain: arming bumps it, firing only
    /// re-arms when the token still matches.
    poll_token: u64,
    /// Ledger-derived display inputs, memoized on `Snapshot::ledger_hash`
    /// so chain builds/walks run only on ledger change.
    ledger_viz: Option<LedgerViz>,
}

impl ProgressView {
    pub fn new(pid: String) -> Self {
        Self {
            service: None,
            pid,
            snapshot: None,
            error: None,
            action_pending: false,
            translating_chapter: None,
            visible: true,
            usage_group: super::widgets::UsageGroup::ByStage,
            poll_token: 0,
            ledger_viz: None,
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

    pub fn set_client(&mut self, service: ServiceHandle, cx: &mut Context<Self>) {
        self.service = Some(service);
        self.refresh(cx);
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let (Some(service), pid) = (self.service.clone(), self.pid.clone()) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { load(&service, &pid) })
                .await;
            _ = this.update(cx, |view: &mut ProgressView, cx| match result {
                Ok(snapshot) => {
                    if view.snapshot.as_ref() != Some(&snapshot) || view.error.is_some() {
                        view.snapshot = Some(snapshot);
                        view.error = None;
                        cx.notify();
                    }
                    // Live follow-up only while busy: re-arm the status ping
                    // (the light leg decides; the heavy snapshot stays cached).
                    view.arm_status_ping(cx);
                }
                Err(err) => {
                    let msg = err.to_string();
                    if view.error.as_ref() != Some(&msg) {
                        view.error = Some(msg);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Light status ping (one cached `get_project`, no snapshot rebuild).
    /// Re-arms every 3s while visible AND busy; any transition (busy state
    /// or data change) triggers a full `refresh`, otherwise the view sits
    /// idle with zero renders. Singularized via `poll_token` like before.
    fn arm_status_ping(&mut self, cx: &mut Context<Self>) {
        self.poll_token = self.poll_token.wrapping_add(1);
        let token = self.poll_token;
        let (Some(service), pid) = (self.service.clone(), self.pid.clone()) else {
            return;
        };
        let last_status = self
            .snapshot
            .as_ref()
            .map(|s| s.project.status.clone())
            .unwrap_or_default();
        let last_hash = self.snapshot.as_ref().map(|s| s.ledger_hash).unwrap_or(0);
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(3))
                .await;
            // Light leg off the UI thread: cached status + ledger fingerprint.
            let probe = cx
                .background_spawn(async move {
                    let detail = service.get_project(&pid).ok()?;
                    let stats = service.get_stats(&pid).ok()?;
                    let hash = ledger_hash(&stats.usage, &stats.timing);
                    Some((detail.project.status, hash))
                })
                .await;
            _ = this.update(cx, |view: &mut ProgressView, cx| {
                if view.poll_token != token || !view.visible {
                    return;
                }
                let Some((status, hash)) = probe else {
                    return;
                };
                let was_busy = is_project_busy(&last_status);
                let is_busy = is_project_busy(&status);
                // Transition or ledger change → full snapshot refresh (which
                // re-arms this ping). Steady busy → re-arm the light ping
                // without touching the snapshot (no render).
                if status != last_status || hash != last_hash {
                    view.refresh(cx);
                } else if was_busy && is_busy {
                    view.arm_status_ping(cx);
                }
                // Idle + unchanged: stop. `set_visible` resumes on revisit.
            });
        })
        .detach();
    }

    fn run_action(&mut self, kind: &str, cx: &mut Context<Self>) {
        let (Some(service), pid) = (self.service.clone(), self.pid.clone()) else {
            return;
        };
        let kind = kind.to_string();
        self.action_pending = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    match kind.as_str() {
                        "pause" => service.pause(&pid),
                        "resume" => service.resume(&pid),
                        _ => service.translate(&pid),
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

    fn translate_chapter(&mut self, ci: u32, cx: &mut Context<Self>) {
        let (Some(service), pid) = (self.service.clone(), self.pid.clone()) else {
            return;
        };
        self.translating_chapter = Some(ci);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { service.translate_chapter(&pid, ci) })
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

    /// Chapters panel: compact rows (title + words + status pill +
    /// per-chapter translate) in a fixed-height scroll container, so long
    /// chapter lists never extend the page. The translate button only shows
    /// for chapters that are not done (mirrors the source condition).
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
                        .child(div().text_xs().text_color(palette.muted_foreground).child(
                            format!(
                                "{}/{} words · {}",
                                ch.target_word_count,
                                ch.word_count,
                                Self::review_badge(ch),
                            ),
                        )),
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
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child("Chapters"),
            )
            .child(div().h(px(320.0)).overflow_y_scrollbar().child(list))
    }

    /// Workflow section: kind + status header, then one row per stage
    /// (number square + label + state pill). No card frame — the panel
    /// provides the fill.
    fn workflow_section(
        workflow: Option<&WorkflowOut>,
        palette: &theme::Palette,
    ) -> impl IntoElement {
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
                                this.bg(palette.success.opacity(0.15))
                                    .text_color(palette.success)
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
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Current workflow"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(workflow.kind.clone()),
                            )
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
        let mut col = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_3()
            .p_6()
            .overflow_y_scrollbar()
            .font(theme::fonts::ui());

        if self.snapshot.is_none() {
            col = col.child(super::ui::page_title("Translation overview", None::<Div>));
            if let Some(err) = &self.error {
                col = col.child(super::ui::error_box(err, &palette));
            } else {
                col = col.child(
                    div()
                        .text_sm()
                        .text_color(palette.muted_foreground)
                        .child("Loading…"),
                );
            }
            return col;
        }

        let busy = is_project_busy(
            &self
                .snapshot
                .as_ref()
                .expect("snapshot checked")
                .project
                .status,
        );
        let ledger_hash = self
            .snapshot
            .as_ref()
            .expect("snapshot checked")
            .ledger_hash;
        let stale = self
            .ledger_viz
            .as_ref()
            .is_none_or(|viz| viz.hash != ledger_hash);
        if stale {
            let snapshot = self.snapshot.as_ref().expect("snapshot checked");
            self.ledger_viz = Some(LedgerViz {
                hash: snapshot.ledger_hash,
                metrics: super::widgets::UsageMetrics::from_parts(
                    &snapshot.usage,
                    &snapshot.timing,
                    busy,
                ),
                runs: super::widgets::timing_runs(&snapshot.timing),
            });
        }
        let snapshot = self.snapshot.as_ref().expect("snapshot checked");
        let project = &snapshot.project;
        let paused = project.status == "paused";
        // Segment counts, not finished chapters (mirrors source comment).
        let done: u64 = snapshot.chapters.iter().map(|c| c.target_word_count).sum();
        let total: u64 = snapshot
            .chapters
            .iter()
            .map(|c| c.word_count)
            .sum::<u64>()
            .max(snapshot.project_total_words);
        let pct = if total > 0 {
            ((done as f64 / total as f64) * 100.0).round() as f32
        } else {
            0.0
        };

        // Header with exact visibility conditions from ProgressPage.
        let caption = format!(
            "{} → {} · {}",
            project.source_lang.as_deref().unwrap_or("auto"),
            project.target_lang.as_deref().unwrap_or("—"),
            project.title.as_deref().unwrap_or("waiting for source"),
        );
        let mut header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(super::ui::page_title(project.name.clone(), Some(caption)));
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
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Translation overview"),
                    )
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

        // Usage + run history (engine ledgers; empty states when no runs).
        // `usage`/`timing` walks are memoized on the ledger hash above (only
        // the group tabs re-render cheaply on `usage_group` switch).
        let viz = self.ledger_viz.clone().expect("ledger_viz memoized");
        let has_usage = viz.metrics.total_tokens.is_some() || viz.metrics.calls.is_some();
        if has_usage {
            overview = overview.child(super::widgets::metric_cards(&viz.metrics, &palette));
            // Underline tabs (same pattern as glossary/settings).
            let group = self.usage_group;
            let selected = super::widgets::UsageGroup::ALL
                .iter()
                .position(|g| *g == group)
                .unwrap_or(0);
            let mut tabs = TabBar::new("usage-group-tabs")
                .underline()
                .selected_index(selected)
                .on_click(cx.listener(|this, index: &usize, _, cx| {
                    if let Some(g) = super::widgets::UsageGroup::ALL.get(*index) {
                        this.usage_group = *g;
                        cx.notify();
                    }
                }));
            for g in super::widgets::UsageGroup::ALL {
                tabs = tabs.child(Tab::new().label(g.tab_label()));
            }
            overview = overview.child(tabs);
            let usage_viz = match group {
                super::widgets::UsageGroup::ByStage => {
                    super::widgets::usage_rows(&snapshot.usage, group, &palette).into_any_element()
                }
                super::widgets::UsageGroup::ByModel => {
                    super::widgets::usage_donut(&snapshot.usage, &palette).into_any_element()
                }
                super::widgets::UsageGroup::ByProvider => {
                    super::widgets::usage_provider_donut(&snapshot.usage, &palette)
                        .into_any_element()
                }
            };
            overview = overview.child(usage_viz);
        }
        if !viz.runs.is_empty() {
            overview = overview.child(super::widgets::run_chart(&viz.runs, &palette));
        }
        // Review runs (engine review history; hidden when never reviewed).
        if !snapshot.review_runs.is_empty() {
            let mut review = div().flex().flex_col().gap_2().child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child("Review runs"),
            );
            for run in snapshot.review_runs.iter().take(5) {
                review = review.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_sm()
                        .child(div().flex_1().min_w_0().truncate().child(run.id.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(palette.muted_foreground)
                                .child(format!("{} issues", run.issues.len())),
                        )
                        .child(
                            div()
                                .px_2()
                                .rounded_sm()
                                .bg(palette.muted)
                                .text_xs()
                                .child(run.status.clone()),
                        ),
                );
            }
            overview = overview.child(review);
        }
        // Chapters panel below the overview progress.
        col = col.child(overview.child(self.chapter_panel(&snapshot.chapters, busy, &palette, cx)));

        // Not-initialized note. The source links to the create screen with a
        // project prefill the native create screen has no API for, so this
        // stays a plain note rather than a dead link.
        if !project.initialized {
            col = col.child(
                div()
                    .text_sm()
                    .text_color(palette.muted_foreground)
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
