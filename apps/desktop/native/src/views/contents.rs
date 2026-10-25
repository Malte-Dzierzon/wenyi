//! Contents (screen 6): chapter list + translated-title editor.
//!
//! Parity source: `features/contents/ContentsPage.tsx`, `ContentsList.tsx`,
//! `ContentsEditor.tsx`.
//! - Client-side search filter over `listChapters`.
//! - Edit-title dialog: save/cancel, stale reload (`expected_title_translated`
//!   guard), empty guard, read-only when busy.
//! - Read-contents link: React links each chapter to the translation source;
//!   natively each row offers "Open in proofreading" via the
//!   `on_open_proofreading` callback the App pass wires.
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Disableable, StyledExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::{
    client::{block_on, is_project_busy, ChapterSummary, Client},
    theme,
};

pub struct ContentsView {
    client: Option<Client>,
    pid: String,
    project_status: String,
    chapters: Vec<ChapterSummary>,
    loading: bool,
    error: Option<String>,
    search: Entity<InputState>,
    /// Editing state: (chapter index, baseline translated title).
    editing: Option<(u32, Option<String>)>,
    edit_input: Option<Entity<InputState>>,
    edit_error: Option<String>,
    saving: bool,
    /// Cross-screen link into Proofreading, set by the App pass.
    on_open_proofreading: Option<std::rc::Rc<dyn Fn(u32)>>,
}

impl ContentsView {
    pub fn new(pid: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search chapters…"));
        Self {
            client: None,
            pid,
            project_status: String::new(),
            chapters: Vec::new(),
            loading: true,
            error: None,
            search,
            editing: None,
            edit_input: None,
            edit_error: None,
            saving: false,
            on_open_proofreading: None,
        }
    }

    /// Set the cross-screen link into Proofreading (chapter → open chapter).
    /// Wired by the App pass; rows fall back to edit-only without it.
    pub fn on_open_proofreading(mut self, f: impl Fn(u32) + 'static) -> Self {
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
        self.loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let chapters = block_on(client.list_chapters(&pid))?;
                    let project = block_on(client.get_project(&pid))?;
                    Ok::<_, crate::client::Error>((chapters, project.project.status))
                })
                .await;
            _ = this.update(cx, |view: &mut ContentsView, cx| {
                view.loading = false;
                match result {
                    Ok((chapters, status)) => {
                        view.chapters = chapters;
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

    fn open_edit(&mut self, ch: &ChapterSummary, window: &mut Window, cx: &mut Context<Self>) {
        let baseline = ch.title_translated.clone();
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("Translated title");
            state.set_value(baseline.clone().unwrap_or_default(), window, cx);
            state
        });
        self.edit_input = Some(input);
        self.editing = Some((ch.index, baseline));
        self.edit_error = None;
        cx.notify();
    }

    fn save_edit(&mut self, cx: &mut Context<Self>) {
        let (Some(client), Some((ci, baseline)), Some(input)) = (
            self.client.clone(),
            self.editing.clone(),
            self.edit_input.clone(),
        ) else {
            return;
        };
        let title: String = input.read(cx).value().to_string();
        if title.trim().is_empty() {
            self.edit_error = Some("Translated title must not be empty".into());
            cx.notify();
            return;
        }
        let pid = self.pid.clone();
        self.saving = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    block_on(client.update_chapter_title(&pid, ci, title.trim(), baseline.as_deref()))
                })
                .await;
            _ = this.update(cx, |view: &mut ContentsView, cx| {
                view.saving = false;
                match result {
                    Ok(_) => {
                        view.editing = None;
                        view.edit_input = None;
                        view.refresh(cx);
                    }
                    Err(err) => {
                        // 409 = stale (someone else saved first) — mirrors the
                        // stale-reload affordance; keep dialog open with error.
                        view.edit_error = Some(err.to_string());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn filtered(&self, cx: &App) -> Vec<ChapterSummary> {
        let q = self.search.read(cx).value().to_string().to_lowercase();
        if q.trim().is_empty() {
            return self.chapters.clone();
        }
        self.chapters
            .iter()
            .filter(|ch| {
                ch.title.to_lowercase().contains(&q)
                    || ch.title_translated.as_deref().unwrap_or("").to_lowercase().contains(&q)
            })
            .cloned()
            .collect()
    }
}

impl Render for ContentsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current_palette();
        let busy = self.busy();
        // Scroll region owns overflow (scrollbar overlay); flex_1 + min_h_0
        // lets it shrink inside the app-shell column.
        let mut col = div().flex().flex_col().flex_1().min_h_0().gap_3().p_6()
            .overflow_y_scrollbar()
            .child(
                div().text_xl().font_weight(FontWeight::BOLD).child("Contents"),
            );

        if busy {
            col = col.child(
                div().text_sm().text_color(palette.muted_foreground)
                    .child("Project is running — titles are read-only."),
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
                    Button::new("contents-search")
                        .label("Search")
                        .on_click(cx.listener(|this, _, _, cx| cx.notify())),
                ),
        );

        // Edit-title panel (inline edit affordance; Esc cancels via the key
        // handler on the panel below).
        if let Some((ci, _baseline)) = &self.editing {
            let ci = *ci;
            col = col.child(
                div().flex().flex_col().gap_2().p_4().rounded_md()
                    .border_1().border_color(palette.ring)
                    .on_key_down(cx.listener(|this, ev: &gpui_kit::KeyDownEvent, _, cx| {
                        // Esc cancels the edit (modal dialogs also close on
                        // Esc via the kit default); Tab order stays native.
                        if ev.keystroke.key == "escape" {
                            this.editing = None;
                            this.edit_input = None;
                            cx.notify();
                        }
                    }))
                    .child(
                        match &self.edit_input {
                            Some(input) => Input::new(input).into_any_element(),
                            None => div().into_any_element(),
                        },
                    )
                    .child(
                        div().flex().gap_2()
                            .child(
                                Button::new("save-title")
                                    .primary()
                                    .label(if self.saving { "Saving…" } else { "Save" })
                                    .disabled(self.saving || busy)
                                    .on_click(cx.listener(|this, _, _, cx| this.save_edit(cx))),
                            )
                            .child(
                                Button::new("cancel-title").ghost().label("Cancel").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.editing = None;
                                        this.edit_input = None;
                                        cx.notify();
                                    }),
                                ),
                            )
                            .child(
                                Button::new("reload-title")
                                    .ghost()
                                    .label("Load latest")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.editing = None;
                                        this.edit_input = None;
                                        this.refresh(cx);
                                    })),
                            ),
                    ),
            );
            if let Some(err) = &self.edit_error {
                col = col.child(div().text_sm().text_color(palette.destructive).child(err.clone()));
            }
        }

        if self.loading && self.chapters.is_empty() {
            col = col.child(div().text_sm().text_color(palette.muted_foreground).child("Loading…"));
        } else {
            let chapters = self.filtered(cx);
            if chapters.is_empty() {
                col = col.child(
                    div().py_12().text_center().text_sm().text_color(palette.muted_foreground)
                        .child("No chapters match."),
                );
            } else {
                let mut list = div().flex().flex_col().gap_1();
                list = list.child(
                    div().flex().gap_2().text_xs().font_medium()
                        .text_color(palette.muted_foreground)
                        .child(div().w_16().child("#"))
                        .child(div().flex_1().child("Source title"))
                        .child(div().flex_1().child("Translated title"))
                        .child(div().w_24().child("")),
                );
                for ch in chapters {
                    let ch2 = ch.clone();
                    let ci = ch.index;
                    let mut row = div().flex().gap_2().items_center().text_sm()
                        .p_2().rounded_md().border_1().border_color(palette.border)
                        .child(div().w_16().child(format!("#{ci}")))
                        .child(div().flex_1().child(ch.title.clone()))
                        .child(
                            div().flex_1().text_color(palette.muted_foreground).child(
                                ch.title_translated.clone().unwrap_or_else(|| "—".into()),
                            ),
                        )
                        .child(
                            Button::new(format!("edit-title-{ci}"))
                                .ghost().label("Edit")
                                .disabled(busy)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_edit(&ch2, window, cx);
                                })),
                        );
                    if let Some(cb) = self.on_open_proofreading.clone() {
                        row = row.child(
                            Button::new(format!("proof-title-{ci}"))
                                .ghost().label("Proofreading")
                                .on_click(move |_, _, _| cb(ci)),
                        );
                    }
                    list = list.child(row);
                }
                col = col.child(list);
            }
        }
        col
    }
}
