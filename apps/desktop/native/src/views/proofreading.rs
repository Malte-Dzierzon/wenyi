//! Proofreading (screen 8): chapter list + paragraph rows + editor.
//!
//! Parity source: `features/proofreading/` (Page, ChapterProofreading,
//! ParagraphEditor, PrecisionDrafts, RevisionHistory, ParagraphActions).
//! U-scope: chapter list (search + status filter), paragraph rows
//! (source/target, readOnly/sourceOnly), editor panel (edit tab + save with
//! stale guard, history tab, precision tab with conditional poll).
//! Review deep-links land here via `open_deep_link(ci, si)` (wired by the
//! App pass): the target segment row gets a "linked from review" marker.
//! Deferred to the dialog pass: modal `<dialog>` semantics, context menu,
//! TranslationDiff view.
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Disableable, IndexPath, StyledExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::{
    client::{block_on, is_project_busy, ChapterSegments, ChapterSummary, Client, SegmentRevision},
    theme,
};

pub struct ProofreadingView {
    client: Option<Client>,
    pid: String,
    project_status: String,
    chapters: Vec<ChapterSummary>,
    loading_chapters: bool,
    error: Option<String>,
    search: Entity<InputState>,
    /// Status filter code ("" = all), driven by the filter select.
    status_filter: String,
    status_select: Option<Entity<SelectState<Vec<String>>>>,
    /// Options the select was built with ("" + status codes); rebuilt when
    /// the chapter statuses change.
    status_options: Vec<String>,
    open_ci: Option<u32>,
    chapter: Option<ChapterSegments>,
    /// Virtualized segment list state (variable heights; Glossary uses
    /// `uniform_list` but paragraph rows vary). Reset when the chapter loads.
    seg_list: ListState,
    loading_chapter: bool,
    /// Editing segment (si) + tab + state.
    editing_si: Option<u32>,
    edit_tab: usize,
    edit_input: Option<Entity<InputState>>,
    edit_baseline: Option<String>,
    edit_error: Option<String>,
    saving: bool,
    history: Vec<SegmentRevision>,
    history_loading: bool,
    drafts: Option<serde_json::Value>,
    drafts_loading: bool,
    /// Deep-link highlight set by `open_deep_link` (the `?segment=`
    /// equivalent): the target segment row gets a marker until the user
    /// opens another chapter or editor.
    deep_link_si: Option<u32>,
}

impl ProofreadingView {
    pub fn new(pid: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search chapters…"));
        Self {
            client: None,
            pid,
            project_status: String::new(),
            chapters: Vec::new(),
            loading_chapters: true,
            error: None,
            search,
            status_filter: String::new(),
            status_select: None,
            status_options: Vec::new(),
            open_ci: None,
            chapter: None,
            seg_list: ListState::new(0, ListAlignment::Top, px(400.0)),
            loading_chapter: false,
            editing_si: None,
            edit_tab: 0,
            edit_input: None,
            edit_baseline: None,
            edit_error: None,
            saving: false,
            history: Vec::new(),
            history_loading: false,
            drafts: None,
            drafts_loading: false,
            deep_link_si: None,
        }
    }

    pub fn set_client(&mut self, client: Client, cx: &mut Context<Self>) {
        self.client = Some(client);
        self.refresh_chapters(cx);
    }

    fn busy(&self) -> bool {
        is_project_busy(&self.project_status)
    }

    fn refresh_chapters(&mut self, cx: &mut Context<Self>) {
        let (Some(client), pid) = (self.client.clone(), self.pid.clone()) else {
            return;
        };
        self.loading_chapters = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let (chapters, project) = block_on(async {
                        tokio::join!(client.list_chapters(&pid), client.get_project(&pid),)
                    });
                    Ok::<_, crate::client::Error>((chapters?, project?.project.status))
                })
                .await;
            _ = this.update(cx, |view: &mut ProofreadingView, cx| {
                view.loading_chapters = false;
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
    fn open_chapter(&mut self, ci: u32, cx: &mut Context<Self>) {
        let (Some(client), pid) = (self.client.clone(), self.pid.clone()) else {
            return;
        };
        self.open_ci = Some(ci);
        self.chapter = None;
        self.editing_si = None;
        self.deep_link_si = None;
        self.loading_chapter = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result =
                cx.background_spawn(async move { block_on(client.get_review_chapter(&pid, ci)) })
                    .await;
            _ = this.update(cx, |view: &mut ProofreadingView, cx| {
                view.loading_chapter = false;
                match result {
                    Ok(chapter) => {
                        view.seg_list.reset(chapter.segments.len());
                        view.chapter = Some(chapter);
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

    /// One virtualized paragraph row (source | translation + edit). Reads the
    /// segment by index — no chapter clone, only visible rows are built.
    /// Called inside `view.update` from the list item closure, so `Context`
    /// (and `cx.listener`) is available.
    fn segment_row(
        view: &ProofreadingView,
        ix: usize,
        palette: theme::Palette,
        cx: &mut Context<ProofreadingView>,
    ) -> AnyElement {
        let (Some(chapter), busy) = (view.chapter.as_ref(), view.busy()) else {
            return div().into_any_element();
        };
        let (Some(seg), last) = (
            chapter.segments.get(ix),
            chapter.segments.len().saturating_sub(1),
        ) else {
            return div().into_any_element();
        };
        let si = seg.index;
        let linked = view.deep_link_si == Some(si);
        let baseline = seg.target.clone();
        div()
            .flex()
            .w_full()
            .when(ix != last, |this| {
                this.border_b_1().border_color(palette.border)
            })
            .when(linked, |this| this.bg(palette.accent_soft))
            // Source column.
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .p_4()
                    .border_r_1()
                    .border_color(palette.border)
                    .child(
                        div().text_xs().text_color(palette.muted_foreground).child(
                            if linked {
                                format!("#{si} ← linked from review")
                            } else {
                                format!("#{si}")
                            },
                        ),
                    )
                    .child(div().text_sm().child(seg.source.clone())),
            )
            // Translation column + edit action.
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div().text_xs().text_color(palette.muted_foreground).child("Translation"),
                    )
                    .child(
                        div().text_sm().child(
                            seg.target.clone().unwrap_or_else(|| "(untranslated)".into()),
                        ),
                    )
                    .child(
                        Button::new(format!("edit-seg-{si}"))
                            .ghost()
                            .label("Edit")
                            .disabled(busy)
                            .on_click({
                                cx.listener(move |this, _, window, cx| {
                                    this.open_editor(si, baseline.clone(), window, cx);
                                })
                            }),
                    ),
            )
            .into_any_element()
    }

    /// Cross-screen entry point for Review issue links (the `?segment=`
    /// deep-link equivalent). Opens the chapter and highlights the segment;
    /// if the chapter is already open, only the highlight moves. Callable
    /// from the App pass via the entity handle.
    pub fn open_deep_link(&mut self, ci: u32, si: Option<u32>, cx: &mut Context<Self>) {
        if self.open_ci == Some(ci) && (self.chapter.is_some() || self.loading_chapter) {
            self.deep_link_si = si;
            cx.notify();
            return;
        }
        self.deep_link_si = si;
        self.open_chapter(ci, cx);
    }

    fn open_editor(
        &mut self,
        si: u32,
        baseline: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("Translation");
            state.set_value(baseline.clone().unwrap_or_default(), window, cx);
            state
        });
        self.edit_input = Some(input);
        self.editing_si = Some(si);
        self.edit_tab = 0;
        self.edit_baseline = baseline;
        self.edit_error = None;
        self.history.clear();
        self.drafts = None;
        cx.notify();
    }

    fn save_edit(&mut self, cx: &mut Context<Self>) {
        let (Some(client), Some(ci), Some(si), Some(input)) = (
            self.client.clone(),
            self.open_ci,
            self.editing_si,
            self.edit_input.clone(),
        ) else {
            return;
        };
        let target: String = input.read(cx).value().to_string();
        let baseline = self.edit_baseline.clone();
        let pid = self.pid.clone();
        self.saving = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    block_on(client.edit_segment(&pid, ci, si, &target, baseline.as_deref()))
                })
                .await;
            _ = this.update(cx, |view: &mut ProofreadingView, cx| {
                view.saving = false;
                match result {
                    Ok(_) => {
                        view.editing_si = None;
                        view.edit_input = None;
                        view.open_chapter(ci, cx);
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

    fn load_history(&mut self, cx: &mut Context<Self>) {
        let (Some(client), Some(ci), Some(si)) =
            (self.client.clone(), self.open_ci, self.editing_si)
        else {
            return;
        };
        let pid = self.pid.clone();
        self.history_loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { block_on(client.segment_history(&pid, ci, si)) })
                .await;
            _ = this.update(cx, |view: &mut ProofreadingView, cx| {
                view.history_loading = false;
                match result {
                    Ok(history) => {
                        view.history = history;
                    }
                    Err(err) => {
                        view.edit_error = Some(err.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_drafts(&mut self, cx: &mut Context<Self>) {
        let (Some(client), Some(ci), Some(si)) =
            (self.client.clone(), self.open_ci, self.editing_si)
        else {
            return;
        };
        let pid = self.pid.clone();
        self.drafts_loading = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { block_on(client.precision_drafts(&pid, ci, si)) })
                .await;
            _ = this.update(cx, |view: &mut ProofreadingView, cx| {
                view.drafts_loading = false;
                match result {
                    Ok(drafts) => {
                        view.drafts = Some(drafts);
                    }
                    Err(err) => {
                        // Unavailable reasons (no_archive/incomplete) are
                        // normal before precision runs — show, don't fail.
                        view.drafts = Some(serde_json::json!({"unavailable": err.to_string()}));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn filtered_chapters(&self, cx: &App) -> Vec<ChapterSummary> {
        let q = self.search.read(cx).value().to_string().to_lowercase();
        let q = q.trim();
        self.chapters
            .iter()
            .filter(|ch| {
                (self.status_filter.is_empty() || ch.status == self.status_filter)
                    && (q.is_empty()
                        || format!(
                            "{} {}",
                            ch.title,
                            ch.title_translated.as_deref().unwrap_or("")
                        )
                        .to_lowercase()
                        .contains(q))
            })
            .cloned()
            .collect()
    }
}

impl Render for ProofreadingView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current_palette();
        let busy = self.busy();
        // Scroll region owns overflow (scrollbar overlay); flex_1 + min_h_0
        // lets it shrink inside the app-shell column. Tab order: search →
        // chapter buttons → segment Edit buttons → editor panel.
        let mut col = div().flex().flex_col().flex_1().min_h_0().gap_3().p_6()
            .overflow_y_scrollbar().child(
            div().text_xl().font_weight(FontWeight::BOLD).child("Manual proofreading"),
        );

        if let Some(err) = &self.error {
            col = col.child(
                div().p_3().rounded_md().bg(palette.destructive)
                    .text_color(palette.destructive_foreground)
                    .child(format!("Error: {err}")),
            );
        }

        // Chapter card (mirrors the source `Card`): title + search/filter row
        // + divide-y chapter rows (title, saved words, status pill). Hidden
        // while a chapter is open (the source navigates to a chapter route;
        // the nav row below provides back/prev/next).
        if self.open_ci.is_none() {
        let mut statuses: Vec<String> = Vec::new();
        for ch in &self.chapters {
            if !statuses.contains(&ch.status) {
                statuses.push(ch.status.clone());
            }
        }
        let options = std::iter::once(String::new()).chain(statuses.into_iter()).collect::<Vec<_>>();
        if self.status_select.is_none() || self.status_options != options {
            let idx = options.iter().position(|o| o == &self.status_filter).unwrap_or(0);
            let entity = cx.new(|cx| SelectState::new(options.clone(), Some(IndexPath::new(idx)), window, cx));
            cx.subscribe(
                &entity,
                |this: &mut ProofreadingView, _, event: &SelectEvent<Vec<String>>, cx| {
                    if let SelectEvent::Confirm(Some(value)) = event {
                        this.status_filter = value.clone();
                        cx.notify();
                    }
                },
            )
            .detach();
            self.status_options = options;
            self.status_select = Some(entity);
        }
        let mut card = div()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_md()
            .border_1()
            .border_color(palette.border)
            .bg(palette.card)
            .child(div().font_weight(FontWeight::MEDIUM).child("Proofread by chapter"))
            .child(
                div()
                    .flex()
                    .gap_3()
                    .items_end()
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_sm().child("Search"))
                            .child(Input::new(&self.search)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_sm().child("Filter"))
                            .child(Select::new(self.status_select.as_ref().unwrap())),
                    ),
            );
        if self.loading_chapters && self.chapters.is_empty() {
            card = card.child(
                div().text_sm().text_color(palette.muted_foreground).child("Loading…"),
            );
        } else {
            let chapters = self.filtered_chapters(cx);
            if chapters.is_empty() {
                card = card.child(
                    div().text_sm().text_color(palette.muted_foreground).child(
                        if self.chapters.is_empty() {
                            "Chapters will appear after parsing."
                        } else {
                            "No chapters match the current filters."
                        },
                    ),
                );
            } else {
                let last = chapters.len().saturating_sub(1);
                let mut list = div().flex().flex_col();
                for (i, ch) in chapters.iter().enumerate() {
                    let ci = ch.index;
                    let active = self.open_ci == Some(ci);
                    let title = ch
                        .title_translated
                        .as_deref()
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                        .or_else(|| (!ch.title.trim().is_empty()).then_some(ch.title.trim()))
                        .unwrap_or("Untitled chapter")
                        .to_string();
                    list = list.child(
                        div()
                            .id(format!("open-ch-{ci}"))
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_4()
                            .py_3()
                            .rounded_sm()
                            .when(i != last, |this| {
                                this.border_b_1().border_color(palette.border)
                            })
                            .when(active, |this| this.bg(palette.accent_soft))
                            .hover(|this| this.bg(palette.accent_soft.opacity(0.55)))
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .min_w_0()
                                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
                                    .child(
                                        div().text_xs().text_color(palette.muted_foreground).child(
                                            format!(
                                                "{} / {} words saved",
                                                ch.target_word_count, ch.word_count
                                            ),
                                        ),
                                    ),
                            )
                            .child(
                                div()
                                    .px_2()
                                    .py_1()
                                    .rounded_sm()
                                    .bg(palette.muted)
                                    .text_xs()
                                    .flex_none()
                                    .child(ch.status.clone()),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_chapter(ci, cx);
                            })),
                    );
                }
                card = card.child(list);
            }
        }
        col = col.child(card);
        }
        // Paragraph rows for the open chapter: virtualized `list()` (variable
        // heights) — only visible rows are built, no chapter clone. Same
        // two-column card look as before.
        if self.loading_chapter {
            col = col.child(div().text_sm().text_color(palette.muted_foreground).child("Loading chapter…"));
        } else if self.chapter.is_some() {
            // Chapter navigation (mirrors the source header actions): back to
            // the list + previous/next chapter.
            if let Some(open) = self.open_ci {
                let pos = self.chapters.iter().position(|c| c.index == open);
                let prev = pos.and_then(|p| p.checked_sub(1)).and_then(|p| self.chapters.get(p));
                let next = pos.and_then(|p| self.chapters.get(p + 1));
                let mut nav_row = div().flex().gap_2().child(
                    Button::new("back-to-chapters")
                        .label("All chapters")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.open_ci = None;
                            this.chapter = None;
                            this.editing_si = None;
                            this.deep_link_si = None;
                            cx.notify();
                        })),
                );
                if let Some(prev) = prev {
                    let ci = prev.index;
                    nav_row = nav_row.child(
                        Button::new("prev-chapter")
                            .label("Previous chapter")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_chapter(ci, cx);
                            })),
                    );
                }
                if let Some(next) = next {
                    let ci = next.index;
                    nav_row = nav_row.child(
                        Button::new("next-chapter")
                            .label("Next chapter")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_chapter(ci, cx);
                            })),
                    );
                }
                col = col.child(nav_row);
            }
            let (index, total, saved) = self
                .chapter
                .as_ref()
                .map(|c| {
                    (
                        c.index,
                        c.segments.len(),
                        c.segments.iter().filter(|s| s.target.is_some()).count(),
                    )
                })
                .unwrap_or((0, 0, 0));
            col = col.child(
                div().flex().flex_col().gap_1().child(
                    div().font_medium().child(format!("Chapter #{index} — {total} segments")),
                ).child(
                    div().text_sm().text_color(palette.muted_foreground).child(format!(
                        "{saved} / {total} paragraphs saved",
                    )),
                ),
            );
            if busy {
                col = col.child(
                    div().text_sm().text_color(palette.muted_foreground).child(
                        "Translation is running — editing is disabled until it finishes.",
                    ),
                );
            }
            let view = cx.entity();
            let rows = list(self.seg_list.clone(), move |ix, _window, cx: &mut App| {
                view.update(cx, |view, cx| Self::segment_row(view, ix, palette, cx))
            })
            .flex_1()
            .min_h_0();
            col = col.child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .rounded_md()
                    .border_1()
                    .border_color(palette.border)
                    .bg(palette.card)
                    .overflow_hidden()
                    .child(rows),
            );
        }

        // Editor panel with tabs (edit/history/precision).
        if let Some(si) = self.editing_si {
            let mut panel = div().flex().flex_col().gap_2().p_4().rounded_md()
                .border_1().border_color(palette.ring).bg(palette.card)
                .on_key_down(cx.listener(move |this, ev: &gpui_kit::KeyDownEvent, _, cx| {
                    // Esc cancels the segment edit.
                    if ev.keystroke.key == "escape" {
                        this.editing_si = None;
                        this.edit_input = None;
                        cx.notify();
                    }
                }))
                .child(div().font_medium().child(format!("Edit segment #{si}")));
            panel = panel.child(
                TabBar::new("edit-tabs")
                    .underline()
                    .selected_index(self.edit_tab)
                    .on_click(cx.listener(|this, index: &usize, _, cx| {
                        this.edit_tab = (*index).min(2);
                        if this.edit_tab == 1 {
                            this.load_history(cx);
                        } else if this.edit_tab == 2 {
                            this.load_drafts(cx);
                        } else {
                            cx.notify();
                        }
                    }))
                    .child(Tab::new().label("Edit"))
                    .child(Tab::new().label("History"))
                    .child(Tab::new().label("Precision")),
            );
            match self.edit_tab {
                1 => {
                    panel = panel.child(if self.history_loading {
                        div().text_sm().child("Loading history…").into_any_element()
                    } else if self.history.is_empty() {
                        div().text_sm().text_color(palette.muted_foreground)
                            .child("No revisions yet.").into_any_element()
                    } else {
                        let mut h = div().flex().flex_col().gap_1();
                        for rev in &self.history {
                            h = h.child(div().text_xs().child(format!(
                                "[{}] {} → {}",
                                rev.kind,
                                rev.before.as_deref().unwrap_or("—"),
                                rev.after.as_deref().unwrap_or("—"),
                            )));
                        }
                        h.into_any_element()
                    });
                }
                2 => {
                    panel = panel.child(if self.drafts_loading {
                        div().text_sm().child("Loading drafts…").into_any_element()
                    } else {
                        div().text_xs().child(
                            self.drafts
                                .as_ref()
                                .map(|d| format!("{d}"))
                                .unwrap_or_else(|| "No drafts.".into()),
                        )
                        .into_any_element()
                    });
                }
                _ => {
                    if let Some(input) = &self.edit_input {
                        panel = panel.child(Input::new(input));
                    }
                    panel = panel.child(
                        div().flex().gap_2()
                            .child(
                                Button::new("save-seg")
                                    .primary()
                                    .label(if self.saving { "Saving…" } else { "Save" })
                                    .disabled(self.saving || busy)
                                    .on_click(cx.listener(|this, _, _, cx| this.save_edit(cx))),
                            )
                            .child(
                                Button::new("cancel-seg").ghost().label("Cancel").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.editing_si = None;
                                        this.edit_input = None;
                                        cx.notify();
                                    }),
                                ),
                            ),
                    );
                    if let Some(err) = &self.edit_error {
                        panel = panel.child(
                            div().text_sm().text_color(palette.destructive).child(err.clone()),
                        );
                    }
                }
            }
            col = col.child(panel);
        }
        col
    }
}
