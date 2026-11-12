//! Event log (screen: events): engine progress events, newest first.
//!
//! Parity source: `features/events/EventsPage.tsx`.
//! - Live push via engine `subscribe()` (`started`/`progress`/`done`/
//!   `failed`/`interrupted`); rows cap at 500, newest first.
//! - NO type filter (source exposes none).
//! - Row: type badge (label + tone), expandable payload
//!   (`<details>/<summary>` → `Collapsible`)
//! - States: listening / error / empty.

use std::collections::HashSet;

use gpui_kit::component::collapsible::Collapsible;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::*;
use gpui_kit::{InteractiveElement, StatefulInteractiveElement};

use crate::{
    service::{EngineEvent, EventOut, ServiceHandle},
    theme,
};

/// Badge label + tone. Mirrors `badgeFor` in `EventsPage.tsx`; the exact
/// English strings come from `packages/ui/src/i18n/locales/en.ts`.
fn badge_for(event_type: &str) -> (&str, BadgeTone) {
    match event_type {
        "started" => ("Started", BadgeTone::Info),
        "progress" => ("Progress", BadgeTone::Info),
        "done" => ("Done", BadgeTone::Success),
        "failed" => ("Failed", BadgeTone::Warning),
        "interrupted" => ("Interrupted", BadgeTone::Warning),
        _ => ("Event", BadgeTone::Secondary),
    }
}

#[derive(Clone, Copy)]
enum BadgeTone {
    Info,
    Success,
    Warning,
    Secondary,
}

impl BadgeTone {
    fn color(self, palette: &theme::Palette) -> Hsla {
        match self {
            BadgeTone::Info => palette.info,
            BadgeTone::Success => palette.success,
            BadgeTone::Warning => palette.warning,
            BadgeTone::Secondary => palette.muted_foreground,
        }
    }
}

/// Human-readable one-liner for an engine progress event.
fn describe(event: &EngineEvent) -> String {
    let detail = if event.total > 0 {
        format!(" ({}/{})", event.done, event.total)
    } else if event.done > 0 {
        format!(" ({})", event.done)
    } else {
        String::new()
    };
    if event.message.trim().is_empty() {
        format!("{}{}", event.kind, detail)
    } else {
        format!("{}{} — {}", event.kind, detail, event.message.trim())
    }
}

/// Expandable payload lines for an engine event.
fn event_lines(event: &EngineEvent) -> Vec<String> {
    vec![
        format!("project: {}", event.project_id),
        format!("run: {}", event.run_id),
        format!("kind: {}", event.kind),
        format!("progress: {}/{}", event.done, event.total),
        format!("message: {}", event.message),
    ]
}

pub struct EventsView {
    service: Option<ServiceHandle>,
    pid: String,
    rows: Vec<EventRow>,
    /// Virtualized row state; reset when the row count changes so only
    /// visible rows are built (was: up to 500 plain `div` rows).
    rows_list: ListState,
    /// Content-based row signature for the no-op guard: an unchanged
    /// history load skips `reset` + `notify` entirely.
    rows_sig: (usize, u64, u64),
    /// Highest seen per-project sequence (engine `EngineEvent` has no global
    /// id; the row id is a local counter).
    next_id: i64,
    error: Option<String>,
    expanded: HashSet<i64>,
    /// Whether this screen is currently shown (set by the App shell on
    /// navigation). Stored as a plain flag; live updates arrive via the
    /// engine `subscribe()` pump while visible.
    visible: bool,
}

/// Pre-rendered strings for one event row.
#[derive(Clone)]
struct EventRow {
    id: i64,
    event_type: String,
    timestamp: String,
    summary: String,
    lines: Vec<String>,
}

impl EventRow {
    fn new(id: i64, event: &EngineEvent) -> Self {
        Self {
            id,
            event_type: event.kind.clone(),
            timestamp: String::new(),
            summary: describe(event),
            lines: event_lines(event),
        }
    }

    fn from_stored(id: i64, event: &EventOut) -> Self {
        let summary = match event.payload.get("message").and_then(|v| v.as_str()) {
            Some(message) if !message.trim().is_empty() => {
                format!("{} — {}", event.event_type, message.trim())
            }
            _ => event.event_type.clone(),
        };
        Self {
            id,
            event_type: event.event_type.clone(),
            timestamp: super::widgets::short_time(&event.ts.to_string()),
            summary,
            lines: vec![
                format!("job: {}", event.job_id),
                format!("kind: {}", event.event_type),
                format!("payload: {event}", event = event.payload),
            ],
        }
    }
}

impl EventsView {
    pub fn new(pid: String, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            service: None,
            pid,
            rows: Vec::new(),
            rows_list: ListState::new(0, ListAlignment::Top, px(120.0)),
            rows_sig: (0, 0, 0),
            next_id: 0,
            error: None,
            expanded: HashSet::new(),
            visible: true,
        }
    }

    /// Signature of the current row set for change detection. Content-based
    /// (engine events carry no stable per-project id and the local row
    /// counter grows per load, so raw ids never match across reloads).
    fn rows_signature(rows: &[EventRow]) -> (usize, u64, u64) {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for row in rows {
            row.event_type.hash(&mut hasher);
            row.timestamp.hash(&mut hasher);
            row.summary.hash(&mut hasher);
        }
        let newest = rows.first().map(|r| r.id).unwrap_or(0);
        let oldest = rows.last().map(|r| r.id).unwrap_or(0);
        (
            rows.len(),
            hasher.finish(),
            (newest as u64) ^ ((oldest as u64) << 1),
        )
    }

    /// Replace rows only when the signature changed; resets the virtual
    /// list and notifies. Returns true when a re-render was triggered.
    fn set_rows(&mut self, mut rows: Vec<EventRow>, cx: &mut Context<Self>) -> bool {
        rows.truncate(500);
        let sig = Self::rows_signature(&rows);
        if sig == self.rows_sig {
            return false;
        }
        self.rows = rows;
        self.rows_sig = sig;
        self.rows_list.reset(self.rows.len());
        cx.notify();
        true
    }
    /// Called by the App shell on navigation. Becoming visible resumes live
    /// updates; when the history is empty (or a past load failed) it also
    /// triggers a reload so a stale view never stays blank.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        let became_visible = visible && !self.visible;
        self.visible = visible;
        if became_visible && (self.rows.is_empty() || self.error.is_some()) {
            self.reload_history(cx);
        }
    }

    /// One-shot history load with the no-op guard (`set_rows` skips reset +
    /// notify when nothing changed). Failures surface in `error` instead of
    /// leaving the "Listening…" spinner forever.
    fn reload_history(&mut self, cx: &mut Context<Self>) {
        let (Some(service), pid) = (self.service.clone(), self.pid.clone()) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let history = cx
                .background_spawn(async move { service.list_events(&pid) })
                .await;
            _ = this.update(cx, |view: &mut EventsView, cx| match history {
                Ok(events) => {
                    let mut rows: Vec<EventRow> = events
                        .into_iter()
                        .rev()
                        .map(|event| {
                            view.next_id += 1;
                            EventRow::from_stored(view.next_id, &event)
                        })
                        .collect();
                    // History comes back oldest-first from `rev()`: flip so
                    // newest is at index 0 like the live pump maintains.
                    rows.reverse();
                    // No-op guard: an unchanged history skips reset + notify.
                    view.error = None;
                    view.set_rows(rows, cx);
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

    pub fn set_client(&mut self, service: ServiceHandle, cx: &mut Context<Self>) {
        self.service = Some(service.clone());
        // History first (persisted job events), then the live push pump.
        self.reload_history(cx);
        // `subscribe()` opens a broadcast channel per pump iteration; the
        // pump runs on a background thread and forwards matching events to
        // the UI thread. Resubscribing each iteration keeps the non-Clone
        // receiver inside a single `background_spawn` future.
        let pid = self.pid.clone();
        cx.spawn(async move |this, cx| loop {
            let mut rx = service.subscribe();
            let pid = pid.clone();
            let event = cx
                .background_spawn(async move {
                    loop {
                        match rx.recv().await {
                            Ok(event) => {
                                if event.project_id == pid {
                                    return Some(event);
                                }
                            }
                            Err(_) => return None,
                        }
                    }
                })
                .await;
            let Some(event) = event else { break };
            let stop = this
                .update(cx, |view: &mut EventsView, cx| {
                    if view.visible {
                        view.next_id += 1;
                        let row = EventRow::new(view.next_id, &event);
                        let mut rows = std::mem::take(&mut view.rows);
                        rows.insert(0, row);
                        view.error = None;
                        view.set_rows(rows, cx);
                    }
                    false
                })
                .is_err();
            if stop {
                break;
            }
        })
        .detach();
    }
    fn toggle(&mut self, id: i64, cx: &mut Context<Self>) {
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
        cx.notify();
    }

    /// One virtualized event row (badge + summary + expandable payload).
    /// Reads by index — only visible rows are built.
    fn event_row(
        view: &EventsView,
        ix: usize,
        palette: theme::Palette,
        cx: &mut Context<EventsView>,
    ) -> AnyElement {
        let Some(row) = view.rows.get(ix) else {
            return div().into_any_element();
        };
        let id = row.id;
        let open = view.expanded.contains(&id);
        let (badge_label, tone) = badge_for(&row.event_type);
        let tone = tone.color(&palette);
        let summary_row = div()
            .id(format!("event-{id}"))
            .flex()
            .items_start()
            .gap_3()
            .text_sm()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(palette.separator)
            .cursor_pointer()
            .child(
                div()
                    .text_xs()
                    .text_color(palette.muted_foreground)
                    .child(row.timestamp.clone()),
            )
            .child(
                div()
                    .text_xs()
                    .px_2()
                    .py_0()
                    .rounded_md()
                    .border_1()
                    .border_color(tone)
                    .text_color(tone)
                    .child(badge_label.to_string()),
            )
            .child(div().flex_1().child(format!(
                "{} {}",
                if open { "▾" } else { "▸" },
                row.summary
            )))
            .on_click(cx.listener(move |this, _, _, cx| this.toggle(id, cx)));
        let payload_box = div()
            .mt_3()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(palette.border)
            .flex()
            .flex_col()
            .gap_1()
            .children(row.lines.iter().cloned().map(|line| {
                div()
                    .text_xs()
                    .text_color(palette.muted_foreground)
                    .child(line)
            }));
        Collapsible::new()
            .w_full()
            .open(open)
            .child(summary_row)
            .content(payload_box)
            .into_any_element()
    }
}

impl Render for EventsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current_palette();
        let mut col = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_3()
            .p_6()
            .overflow_y_scrollbar()
            .font(theme::fonts::ui())
            .child(super::ui::page_title("Event log", None::<Div>));
        if let Some(err) = &self.error {
            col = col.child(super::ui::error_box(err, &palette));
        }
        if self.rows.is_empty() && self.error.is_none() {
            return col.child(
                div()
                    .text_sm()
                    .text_color(palette.muted_foreground)
                    .child("Listening for engine events…"),
            );
        }

        let mut card = super::ui::list_container(&palette);

        if self.rows.is_empty() {
            card = card.child(super::ui::empty_state("No events yet.", &palette));
        } else {
            // Virtualized rows (proofreading `seg_list` pattern): only
            // visible rows are built; per-index reads, no row clone.
            let view = cx.entity();
            let rows = list(self.rows_list.clone(), move |ix, _window, cx: &mut App| {
                view.update(cx, |view, cx| Self::event_row(view, ix, palette, cx))
            })
            .flex_1()
            .min_h_0();
            card = card.child(rows);
        }

        col.child(card)
    }
}
