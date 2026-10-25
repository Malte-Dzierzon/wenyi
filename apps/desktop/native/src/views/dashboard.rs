//! Dashboard (screen 1): project list cards + delete.
//!
//! Parity source: `features/dashboard/Dashboard.tsx`.
//! - `GET /projects`, 5s refetch (`refetchInterval: 5000`)
//! - Card: name, title-or-placeholder, StatusBadge, delete (confirm), meta row
//!   (langs → target, fmt, created date)
//! - States: loading / empty / error / busy-delete (spinner on the row)

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Disableable, StyledExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::{InteractiveElement, StatefulInteractiveElement};
use gpui_kit::*;
use crate::{
    client::{is_project_busy, Client, Project},
    theme,
};

/// Status → badge (fill, text). Mirrors `badge.tsx` variants: `secondary`
/// uses the neutral pair; tones use a translucent tint fill with the tone
/// itself as text (ref: `bg-emerald-50 text-emerald-700`, …). Alpha over the
/// card derives the tint in both modes without extra palette roles.
fn status_badge(status: &str, palette: &theme::Palette) -> (Hsla, Hsla) {
    let tone = match status {
        "prepared" | "done" | "completed" | "reviewed" | "applied" | "published" => {
            Some(palette.success)
        }
        "pausing" | "paused" | "interrupted" => Some(palette.warning),
        "failed" | "error" => Some(palette.destructive),
        "running" | "parsing" | "preparing" | "translating" | "reviewing" | "autofixing"
        | "postprocessing" => Some(palette.info),
        _ => None,
    };
    match tone {
        Some(t) => (Hsla { a: 0.12, ..t }, t),
        None => (palette.secondary, palette.secondary_foreground),
    }
}
/// Display the `created_at` timestamp as a plain date. ISO `YYYY-MM-DD…`
/// values render as the date part (mirrors the React `toLocaleDateString`
/// day precision); anything already short — or unparseable — renders raw
/// rather than slicing into a misleading string.
fn display_date(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.len() >= 10
        && trimmed.as_bytes()[4] == b'-'
        && trimmed.as_bytes()[7] == b'-'
        && trimmed[..10].chars().all(|c| c.is_ascii_digit() || c == '-')
    {
        return trimmed[..10].to_string();
    }
    trimmed.to_string()
}

pub struct Dashboard {
    client: Option<Client>,
    projects: Option<Vec<Project>>,
    error: Option<String>,
    deleting: Option<String>,
    /// Project with a confirmed delete in flight (row spinner). The confirm
    /// step itself is the modal `confirm_destructive` dialog — no inline
    /// panel state remains.
    confirm_delete: Option<Project>,
    /// Called when a card body is clicked — mirrors the overlay `<Link>`.
    on_open: Option<std::rc::Rc<dyn Fn(Project, &mut Window, &mut App)>>,
}

impl Dashboard {
    pub fn new() -> Self {
        Self {
            client: None,
            projects: None,
            error: None,
            deleting: None,
            confirm_delete: None,
            on_open: None,
        }
    }

    pub fn on_open(
        mut self,
        f: impl Fn(Project, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_open = Some(std::rc::Rc::new(f));
        self
    }
    pub fn set_client(&mut self, client: Client, cx: &mut Context<Self>) {
        self.client = Some(client);
        self.refresh(cx);
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            // reqwest needs Tokio: run the future on the shared runtime from
            // a background thread, then update the entity on the UI thread.
            let result = cx
                .background_spawn(async move { crate::client::block_on(client.list_projects()) })
                .await;
            _ = this.update(cx, |view: &mut Dashboard, cx| {
                match result {
                    Ok(projects) => {
                        tracing::info!(count = projects.len(), "projects fetched");
                        view.projects = Some(projects);
                        view.error = None;
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "projects fetch failed");
                        view.error = Some(err.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
        // 5s refetch like the React query (`refetchInterval: 5000`).
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(5))
                .await;
            _ = this.update(cx, |view: &mut Dashboard, cx| {
                view.refresh(cx);
            });
        })
        .detach();
    }

    fn request_delete(&mut self, project: Project, window: &mut Window, cx: &mut Context<Self>) {
        // Mirrors `window.confirm(...)` — modal destructive confirm over the
        // kit dialog layer (Esc cancels via kit default). The stashed copy
        // drives the row spinner in `delete_confirmed`.
        self.confirm_delete = Some(project.clone());
        let name = project.name.clone();
        let pid = project.id.clone();
        let this = cx.entity();
        super::confirm_destructive(
            window,
            cx,
            "Delete project",
            format!("Delete project “{name}” (pid {pid})? This cannot be undone."),
            "Delete",
            move |window, cx| {
                this.update(cx, |view: &mut Dashboard, cx| {
                    view.delete_confirmed(window, cx);
                });
                true
            },
        );
    }

    /// Run the confirmed delete (same backend call as `do_delete` before;
    /// toasts the outcome as the pilot for the shared helper API).
    fn delete_confirmed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(client), Some(project)) = (self.client.clone(), self.confirm_delete.take())
        else {
            return;
        };
        let window_handle = window.window_handle();
        self.deleting = Some(project.id.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let pid = project.id.clone();
            let pname = project.name.clone();
            let result: Result<serde_json::Value, _> = cx
                .background_spawn(async move {
                    crate::client::block_on(client.delete_project(&pid))
                })
                .await;
            _ = this.update(cx, |view: &mut Dashboard, cx| {
                view.deleting = None;
                match result {
                    Ok(_) => {
                        cx.update_window(window_handle, |_, window, cx| {
                            super::notify_success(window, cx, format!("Deleted project “{pname}”."));
                        })
                        .ok();
                        view.refresh(cx);
                    }
                    Err(err) => {
                        let msg = format!("Could not delete project: {err}");
                        view.error = Some(msg.clone());
                        cx.update_window(window_handle, |_, window, cx| {
                            super::notify_error(window, cx, msg);
                        })
                        .ok();
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }


    fn render_card(&self, project: &Project, palette: theme::Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let busy_deleting = self.deleting.as_deref() == Some(project.id.as_str());
        let langs = format!(
            "{} → {}",
            project.source_lang.as_deref().unwrap_or("?"),
            project.target_lang.as_deref().unwrap_or("zh")
        );
        let title = project
            .title
            .clone()
            .unwrap_or_else(|| "(No source uploaded)".to_string());
        // Created date: mirrors `new Date(p.created_at).toLocaleDateString()`.
        // ISO `YYYY-MM-DD…` renders as the date part; anything else renders
        // raw rather than slicing into a misleading string.
        let created = project
            .created_at
            .as_deref()
            .map(display_date)
            .filter(|d| !d.is_empty())
            .map(|d| format!(" · {d}"))
            .unwrap_or_default();
        let meta = format!(
            "{langs}{}{}{}",
            project.fmt.as_deref().map(|f| format!(" · {f}")).unwrap_or_default(),
            created,
            if is_project_busy(&project.status) {
                " · busy"
            } else {
                ""
            },
        );
        let delete_label = if busy_deleting { "…" } else { "Delete" };
        let project_for_delete = project.clone();
        let project_for_open = project.clone();
        let on_open = self.on_open.clone();
        div()
            .id(SharedString::from(format!("project-card-{}", project.id)))
            .flex()
            .flex_col()
            .gap_2()
            .p_5()
            .rounded_md()
            .border_1()
            .border_color(palette.border)
            .bg(palette.card)
            .when_some(on_open, |this, on_open| {
                // Card body click = overlay Link in the source. Delete button
                // stops at its own handler (GPUI hit-testing: innermost first).
                this.on_click(move |_, window, cx| on_open(project_for_open.clone(), window, cx))
            })
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().font_medium().child(project.name.clone()))
                            .child(div().text_xs().text_color(palette.muted_foreground).child(title)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .px_2()
                                    .py_px()
                                    .rounded_sm()
                                    .bg(status_badge(&project.status, &palette).0)
                                    .text_color(status_badge(&project.status, &palette).1)
                                    .text_xs()
                                    .child(project.status.clone()),
                            )
                            .child(
                                Button::new(format!("delete-{}", project.id))
                                    .ghost()
                                    .label(delete_label)
                                    .disabled(busy_deleting || self.deleting.is_some())
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.request_delete(project_for_delete.clone(), window, cx);
                                    })),
                            ),
                    ),
            )
            .child(div().text_xs().text_color(palette.muted_foreground).child(meta))
    }
}

impl Render for Dashboard {
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
            .child(div().text_xl().font_weight(FontWeight::BOLD).child("My projects"));

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


        match &self.projects {
            None if self.error.is_none() => {
                col = col.child(
                    div()
                        .text_sm()
                        .text_color(palette.muted_foreground)
                        .child("Loading…"),
                );
            }
            Some(projects) if projects.is_empty() => {
                col = col.child(
                    div()
                        .py_24()
                        .text_center()
                        .text_sm()
                        .text_color(palette.muted_foreground)
                        .child("No projects yet."),
                );
            }
            Some(projects) => {
                let mut grid = div().flex().flex_col().gap_3();
                for project in projects {
                    grid = grid.child(self.render_card(project, palette, cx));
                }
                col = col.child(grid);
            }
            None => {}
        }
        col
    }
}

#[cfg(test)]
mod tests {
    use super::display_date;

    #[test]
    fn display_date_keeps_iso_day_and_passes_raw_through() {
        assert_eq!(display_date("2026-10-25T12:00:00Z"), "2026-10-25");
        assert_eq!(display_date("2026-10-25"), "2026-10-25");
        // Non-ISO values render raw (never a sliced fragment).
        assert_eq!(display_date("Oct 25, 2026"), "Oct 25, 2026");
        assert_eq!(display_date(""), "");
    }
}
