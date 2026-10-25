//! Subtitles (screen 10, srt only): cue cards + edit + pagination.
//!
//! Parity source: `features/subtitles/SubtitlesPage.tsx`.
//! - Header count (completed/total), export link (App-wired `on_open_export`
//!   callback → Export screen; mirrors the React `<Link to={…/export}>`),
//!   client search, cue cards (id, start → end, source,
//!   SegmentEditor-equivalent input), 50/page pagination with bounds-disabled
//!   buttons. Busy disables edits.
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Disableable, StyledExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::{
    client::{block_on, is_project_busy, Client, SubtitleCue},
    theme,
};

const PAGE_SIZE: usize = 50;

pub struct SubtitlesView {
    client: Option<Client>,
    pid: String,
    project_status: String,
    cues: Vec<SubtitleCue>,
    completed: u32,
    total: u32,
    loading: bool,
    error: Option<String>,
    search: Entity<InputState>,
    page: usize,
    editing: Option<String>,
    edit_input: Option<Entity<InputState>>,
    saving: bool,
    edit_error: Option<String>,
    /// Cross-screen link into Export, set by the App pass.
    on_open_export: Option<std::rc::Rc<dyn Fn()>>,
}

impl SubtitlesView {
    pub fn new(pid: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search subtitles…"));
        Self {
            client: None,
            pid,
            project_status: String::new(),
            cues: Vec::new(),
            completed: 0,
            total: 0,
            loading: true,
            error: None,
            search,
            page: 0,
            editing: None,
            edit_input: None,
            saving: false,
            edit_error: None,
            on_open_export: None,
        }
    }

    /// Set the cross-screen link into Export (mirrors the header export link).
    /// Wired by the App pass; the header shows the count without it.
    pub fn on_open_export(mut self, f: impl Fn() + 'static) -> Self {
        self.on_open_export = Some(std::rc::Rc::new(f));
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
        self.loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let list = block_on(client.get_subtitles(&pid))?;
                    let project = block_on(client.get_project(&pid))?;
                    Ok::<_, crate::client::Error>((list, project.project.status))
                })
                .await;
            _ = this.update(cx, |view: &mut SubtitlesView, cx| {
                view.loading = false;
                match result {
                    Ok((list, status)) => {
                        view.cues = list.cues;
                        view.completed = list.completed;
                        view.total = list.total;
                        view.project_status = status;
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

    fn open_edit(&mut self, cue: &SubtitleCue, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("Translation");
            state.set_value(cue.target.clone().unwrap_or_default(), window, cx);
            state
        });
        self.edit_input = Some(input);
        self.editing = Some(cue.id.clone());
        self.edit_error = None;
        cx.notify();
    }

    fn save_edit(&mut self, cx: &mut Context<Self>) {
        let (Some(client), Some(cueid), Some(input)) = (
            self.client.clone(),
            self.editing.clone(),
            self.edit_input.clone(),
        ) else {
            return;
        };
        let target: String = input.read(cx).value().to_string();
        let pid = self.pid.clone();
        self.saving = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { block_on(client.edit_subtitle(&pid, &cueid, &target)) })
                .await;
            _ = this.update(cx, |view: &mut SubtitlesView, cx| {
                view.saving = false;
                match result {
                    Ok(_) => {
                        view.editing = None;
                        view.edit_input = None;
                        view.refresh(cx);
                    }
                    Err(err) => {
                        view.edit_error = Some(err.to_string());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn filtered(&self, cx: &App) -> Vec<SubtitleCue> {
        let q = self.search.read(cx).value().to_string().to_lowercase();
        if q.trim().is_empty() {
            return self.cues.clone();
        }
        self.cues
            .iter()
            .filter(|cue| {
                cue.source.to_lowercase().contains(&q)
                    || cue.target.as_deref().unwrap_or("").to_lowercase().contains(&q)
            })
            .cloned()
            .collect()
    }
}

impl Render for SubtitlesView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current_palette();
        let busy = self.busy();
        let mut header_count = div().flex().items_center().gap_2().child(
            div().text_sm().text_color(palette.muted_foreground).child(format!(
                "{}/{} translated",
                self.completed, self.total
            )),
        );
        if let Some(cb) = self.on_open_export.clone() {
            header_count = header_count.child(
                Button::new("goto-export")
                    .label("Export SRT")
                    .on_click(move |_, _, _| cb()),
            );
        }
        let mut col = div().flex().flex_col().flex_1().min_h_0().gap_3().p_6()
            .overflow_y_scrollbar().child(
            div().flex().items_center().justify_between().child(
                div().text_xl().font_weight(FontWeight::BOLD).child("Subtitle editor"),
            ).child(header_count),
        );

        if busy {
            col = col.child(
                div().text_sm().text_color(palette.muted_foreground)
                    .child("Project is running — subtitles are read-only."),
            );
        }
        if let Some(err) = &self.error {
            col = col.child(
                div().p_3().rounded_md().bg(palette.destructive)
                    .text_color(palette.destructive_foreground)
                    .child(format!("Error: {err}")),
            );
        }

        col = col.child(
            div().flex().gap_2()
                .child(div().flex_1().child(Input::new(&self.search)))
                .child(
                    Button::new("cue-search")
                        .label("Search")
                        .on_click(cx.listener(|this, _, _, cx| {
                            // Client-side filter applies on render; reset page.
                            this.page = 0;
                            cx.notify();
                        })),
                ),
        );

        if self.loading && self.cues.is_empty() {
            col = col.child(div().text_sm().text_color(palette.muted_foreground).child("Loading…"));
            return col;
        }

        let filtered = self.filtered(cx);
        if filtered.is_empty() {
            col = col.child(
                div().py_12().text_center().text_sm().text_color(palette.muted_foreground)
                    .child("No matching subtitles."),
            );
            return col;
        }

        let pages = filtered.len().div_ceil(PAGE_SIZE).max(1);
        let page = self.page.min(pages - 1);
        let start = page * PAGE_SIZE;
        let end = (start + PAGE_SIZE).min(filtered.len());
        let mut list = div().flex().flex_col().gap_2();
        for cue in &filtered[start..end] {
            let editing = self.editing.as_deref() == Some(cue.id.as_str());
            let mut card = div().flex().flex_col().gap_1().p_3().rounded_md()
                .border_1().border_color(palette.border)
                .when(editing, |this| {
                    this.border_color(palette.ring).on_key_down(cx.listener(
                        |this, ev: &gpui_kit::KeyDownEvent, _, cx| {
                            // Esc cancels the cue edit; Tab order stays native.
                            if ev.keystroke.key == "escape" {
                                this.editing = None;
                                this.edit_input = None;
                                cx.notify();
                            }
                        },
                    ))
                })
                .child(div().text_xs().font_medium().text_color(palette.muted_foreground).child(
                    format!("#{} · {} → {}", cue.id, cue.start, cue.end),
                ))
                .child(div().text_sm().child(cue.source.clone()));
            if editing {
                if let Some(input) = &self.edit_input {
                    card = card
                        .child(Input::new(input))
                        .child(
                            div().flex().gap_2()
                                .child(
                                    Button::new(format!("save-cue-{}", cue.id))
                                        .primary()
                                        .label(if self.saving { "Saving…" } else { "Save" })
                                        .disabled(self.saving || busy)
                                        .on_click(cx.listener(|this, _, _, cx| this.save_edit(cx))),
                                )
                                .child(
                                    Button::new(format!("cancel-cue-{}", cue.id))
                                        .ghost()
                                        .label("Cancel")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.editing = None;
                                            this.edit_input = None;
                                            cx.notify();
                                        })),
                                ),
                        );
                    if let Some(err) = &self.edit_error {
                        card = card.child(
                            div().text_sm().text_color(palette.destructive).child(err.clone()),
                        );
                    }
                }
            } else {
                card = card
                    .child(
                        div().text_sm().child(
                            cue.target.clone().unwrap_or_else(|| "(untranslated)".into()),
                        ),
                    )
                    .child(
                        Button::new(format!("edit-cue-{}", cue.id))
                            .ghost()
                            .label("Edit")
                            .disabled(busy)
                            .on_click({
                                let cue = cue.clone();
                                cx.listener(move |this, _, window, cx| {
                                    this.open_edit(&cue, window, cx);
                                })
                            }),
                    );
            }
            list = list.child(card);
        }
        col = col.child(list);

        // Pagination prev/next, 50/page, disabled at bounds.
        col = col.child(
            div().flex().items_center().gap_2()
                .child(
                    Button::new("cue-prev")
                        .label("Previous")
                        .disabled(page == 0)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.page = this.page.saturating_sub(1);
                            cx.notify();
                        })),
                )
                .child(div().text_sm().child(format!("Page {} of {pages}", page + 1)))
                .child(
                    Button::new("cue-next")
                        .label("Next")
                        .disabled(page + 1 >= pages)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.page += 1;
                            cx.notify();
                        })),
                ),
        );
        col
    }
}
