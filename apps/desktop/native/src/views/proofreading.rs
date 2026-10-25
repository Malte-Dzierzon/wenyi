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
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Disableable, StyledExt};
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
    /// Open chapter (ci) + loaded segments.
    open_ci: Option<u32>,
    chapter: Option<ChapterSegments>,
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
            open_ci: None,
            chapter: None,
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
                    let chapters = block_on(client.list_chapters(&pid))?;
                    let project = block_on(client.get_project(&pid))?;
                    Ok::<_, crate::client::Error>((chapters, project.project.status))
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
        if q.trim().is_empty() {
            return self.chapters.clone();
        }
        self.chapters
            .iter()
            .filter(|ch| ch.title.to_lowercase().contains(&q))
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

        // Chapter list (left column semantics; single column in U-pass).
        col = col.child(
            div().flex().gap_2()
                .child(div().flex_1().child(Input::new(&self.search)))
                .child(
                    Button::new("proof-search")
                        .label("Search")
                        .on_click(cx.listener(|_, _, _, cx| cx.notify())),
                ),
        );
        if self.loading_chapters && self.chapters.is_empty() {
            col = col.child(div().text_sm().text_color(palette.muted_foreground).child("Loading…"));
        } else {
            let chapters = self.filtered_chapters(cx);
            let mut list = div().flex().gap_1().flex_wrap();
            for ch in chapters {
                let ci = ch.index;
                let active = self.open_ci == Some(ci);
                let mut btn = Button::new(format!("open-ch-{ci}")).label(format!(
                    "#{} {} ({})",
                    ch.index,
                    if ch.title.is_empty() { "—" } else { &ch.title },
                    ch.status
                ));
                if active {
                    btn = btn.primary();
                } else {
                    btn = btn.ghost();
                }
                list = list.child(btn.on_click(
                    cx.listener(move |this, _, _, cx| this.open_chapter(ci, cx)),
                ));
            }
            col = col.child(list);
        }

        // Paragraph rows for the open chapter.
        if self.loading_chapter {
            col = col.child(div().text_sm().text_color(palette.muted_foreground).child("Loading chapter…"));
        } else if let Some(chapter) = self.chapter.clone() {
            col = col.child(
                div().font_medium().child(format!(
                    "Chapter #{} — {} segments",
                    chapter.index,
                    chapter.segments.len()
                )),
            );
            let mut rows = div().flex().flex_col().gap_1();
            for seg in &chapter.segments {
                let si = seg.index;
                let read_only = busy;
                let linked = self.deep_link_si == Some(si);
                let header = if linked {
                    format!("#{si} ← linked from review")
                } else {
                    format!("#{si}")
                };
                rows = rows.child(
                    div().flex().flex_col().gap_1().p_3().rounded_md()
                        .border_1().border_color(palette.border)
                        .child(div().text_xs().text_color(palette.muted_foreground).child(header))
                        .child(div().text_sm().child(seg.source.clone()))
                        .child(
                            div().text_sm().child(
                                seg.target.clone().unwrap_or_else(|| "(untranslated)".into()),
                            ),
                        )
                        .child(
                            Button::new(format!("edit-seg-{si}"))
                                .ghost().label("Edit")
                                .disabled(read_only)
                                .on_click({
                                    let baseline = seg.target.clone();
                                    cx.listener(move |this, _, window, cx| {
                                        this.open_editor(si, baseline.clone(), window, cx);
                                    })
                                }),
                        ),
                );
            }
            col = col.child(rows);
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
                    .selected_index(self.edit_tab)
                    .child(Tab::new().label("Edit"))
                    .child(Tab::new().label("History"))
                    .child(Tab::new().label("Precision")),
            );
            // Tab switching is wired via buttons (TabBar selection callback
            // lands with the tab-interaction pass; index drives content).
            panel = panel.child(
                div().flex().gap_1()
                    .child(
                        Button::new("tab-edit").ghost().label("Edit")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.edit_tab = 0;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("tab-history").ghost().label("History")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.edit_tab = 1;
                                this.load_history(cx);
                            })),
                    )
                    .child(
                        Button::new("tab-precision").ghost().label("Precision")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.edit_tab = 2;
                                this.load_drafts(cx);
                            })),
                    ),
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
