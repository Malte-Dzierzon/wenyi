//! Event log (screen: events): timestamped run events, newest first.
//!
//! Parity source: `features/events/EventsPage.tsx`.
//! - `GET /projects/:pid/events`, 5s refetch (`refetchInterval: 5000`)
//! - Backend returns asc; sort desc by id client-side (`select` in source)
//! - NO type filter (source exposes none; backend filter unused)
//! - Row: timestamp, type badge (label + tone), expandable payload
//!   (`<details>/<summary>` → `Collapsible`)
//! - States: loading / error / empty (source renders `ErrorNotice` + the
//!   `!events?.length` empty branch while errors surface separately)

use std::collections::HashSet;

use gpui_kit::component::collapsible::Collapsible;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::{InteractiveElement, StatefulInteractiveElement};
use gpui_kit::*;

use crate::{
    client::{Client, EventOut},
    theme,
};

/// Badge label + tone. Mirrors `badgeFor` in `EventsPage.tsx`; the exact
/// English strings come from `packages/ui/src/i18n/locales/en.ts`.
fn badge_for(event_type: &str) -> (&str, BadgeTone) {
    match event_type {
        "project" => ("Project", BadgeTone::Info),
        "run_initialized" | "language_detected" | "analysis_saved" | "book_synopsis_saved" => {
            ("Preparation", BadgeTone::Secondary)
        }
        "batch_translated" => ("Translation", BadgeTone::Info),
        "batch_skipped" => ("Translation", BadgeTone::Secondary),
        "chapter_done" => ("Completed", BadgeTone::Success),
        "chapter_reviewed" => ("Review", BadgeTone::Warning),
        "batch_glossary_extracted" => ("Terms", BadgeTone::Secondary),
        "assembled" => ("Export", BadgeTone::Success),
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

fn payload_str(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "—".into(),
        other => {
            let s = other.to_string();
            if s.len() > 200 {
                s.chars().take(200).collect()
            } else {
                s
            }
        }
    }
}

/// Human-readable one-liner. Mirrors `describe(e)` in `EventsPage.tsx`.
fn describe(event: &EventOut) -> String {
    let p = &event.payload;
    let get = |key: &str| {
        p.get(key)
            .map(payload_str)
            .unwrap_or_else(|| "—".into())
    };
    match event.event_type.as_str() {
        "language_detected" => format!("Source language detected: {}", get("source_lang")),
        "analysis_saved" => "Style analysis completed".into(),
        "book_synopsis_saved" => "Book synopsis generated".into(),
        "batch_translated" => format!(
            "Chapter {}: batch completed (paragraphs: {})",
            chapter_one_based(p),
            get("count")
        ),
        "batch_skipped" => format!("Chapter {}: translated batch skipped", chapter_one_based(p)),
        "chapter_done" => format!(
            "Chapter {} “{}” translated",
            chapter_one_based(p),
            get("title")
        ),
        "chapter_reviewed" => format!(
            "Chapter {} — review issues: {}",
            chapter_one_based(p),
            get("issue_count")
        ),
        "assembled" => {
            let files = match p.get("outputs") {
                Some(serde_json::Value::Array(items)) => items
                    .iter()
                    .map(payload_str)
                    .collect::<Vec<_>>()
                    .join(", "),
                other => other.map(payload_str).unwrap_or_default(),
            };
            format!("Exported files: {files}")
        }
        "run_initialized" => format!("Project initialized — chapters: {}", get("chapters")),
        _ => event.event_type.clone(),
    }
}

/// Source adds 1 to the 0-based chapter index (`Number(p.chapter) + 1`).
fn chapter_one_based(payload: &serde_json::Value) -> String {
    match payload.get("chapter") {
        Some(serde_json::Value::Number(n)) => n
            .as_i64()
            .map(|c| (c + 1).to_string())
            .unwrap_or_else(|| n.to_string()),
        other => other.map(payload_str).unwrap_or_else(|| "—".into()),
    }
}

/// Compact `key: value` lines for the expandable payload.
/// Same convention as `compact_json` in `views/progress.rs` (copied, not
/// imported — sibling views stay independent per integration rules).
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

fn payload_lines(payload: &serde_json::Value) -> Vec<String> {
    match payload {
        serde_json::Value::Object(map) if !map.is_empty() => map
            .iter()
            .map(|(k, v)| format!("{k}: {}", payload_str(v)))
            .collect(),
        _ => vec![compact_json(payload)],
    }
}

pub struct EventsView {
    client: Option<Client>,
    pid: String,
    events: Option<Vec<EventOut>>,
    error: Option<String>,
    expanded: HashSet<i64>,
    /// Whether this screen is currently shown; the 5s poll timer only
    /// re-arms while visible (set by the App shell on navigation).
    visible: bool,
}

impl EventsView {
    pub fn new(pid: String, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            client: None,
            pid,
            events: None,
            error: None,
            expanded: HashSet::new(),
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

    pub fn set_client(&mut self, client: Client, cx: &mut Context<Self>) {
        self.client = Some(client);
        self.refresh(cx);
    }

    fn toggle(&mut self, id: i64, cx: &mut Context<Self>) {
        if !self.expanded.remove(&id) {
            self.expanded.insert(id);
        }
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let (Some(client), pid) = (self.client.clone(), self.pid.clone()) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            // reqwest needs Tokio: run the future on the shared runtime from
            // a background thread, then update the entity on the UI thread.
            let result = cx
                .background_spawn(async move { crate::client::block_on(client.list_events(&pid)) })
                .await;
            _ = this.update(cx, |view: &mut EventsView, cx| {
                match result {
                    Ok(mut events) => {
                        // Backend returns asc; newest first (mirrors `select`).
                        events.sort_by(|a, b| b.id.cmp(&a.id));
                        if view.events.as_ref() != Some(&events) || view.error.is_some() {
                            view.events = Some(events);
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
        // 5s refetch like the React query (`refetchInterval: 5000`).
        // Only re-arms while visible; navigation resumes via `set_visible`.
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(5))
                .await;
            _ = this.update(cx, |view: &mut EventsView, cx| {
                if view.visible {
                    view.refresh(cx);
                }
            });
        })
        .detach();
    }
}

impl Render for EventsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current_palette();
        // Scroll region owns overflow; flex_1 + min_h_0 lets it shrink in
        // the app-shell column. Payloads expand via Collapsible (native
        // <details> equivalent, keyboard-togglable by default).
        let mut col = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_3()
            .p_6()
            .overflow_y_scrollbar()
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::BOLD)
                    .child("Event log"),
            );

        // ErrorNotice equivalent (rendered above the card, like the source).
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

        let Some(events) = &self.events else {
            // Loading branch: `useQuery` pending (error shown above if set).
            if self.error.is_none() {
                col = col.child(
                    div()
                        .text_sm()
                        .text_color(palette.muted_foreground)
                        .child("Loading…"),
                );
            }
            return col;
        };

        let mut card = div()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .rounded_md()
            .border_1()
            .border_color(palette.border)
            .bg(palette.card);

        if events.is_empty() {
            card = card.child(
                div()
                    .text_sm()
                    .text_color(palette.muted_foreground)
                    .text_center()
                    .py_8()
                    .child("No events yet."),
            );
        } else {
            for event in events {
                let id = event.id;
                let open = self.expanded.contains(&id);
                let (badge_label, tone) = badge_for(&event.event_type);
                let tone = tone.color(&palette);
                let timestamp = event.created_at.clone().unwrap_or_default();
                let summary = describe(event);
                let lines = payload_lines(&event.payload);

                let summary_row = div()
                    .id(format!("event-{id}"))
                    .flex()
                    .items_start()
                    .gap_3()
                    .text_sm()
                    .py_1()
                    .cursor_pointer()
                    .child(div().text_xs().text_color(palette.muted_foreground).child(timestamp))
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
                    .child(div().flex_1().child(format!("{} {}", if open { "▾" } else { "▸" }, summary)))
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
                    .children(
                        lines
                            .into_iter()
                            .map(|line| div().text_xs().text_color(palette.muted_foreground).child(line)),
                    );

                card = card.child(
                    Collapsible::new()
                        .w_full()
                        .open(open)
                        .child(summary_row)
                        .content(payload_box),
                );
            }
        }

        col.child(card)
    }
}
