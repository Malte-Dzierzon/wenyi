//! Book knowledge (screen 5): synopsis, style voice, cast & terms, chapter digests.
//!
//! One scrollable overview instead of three tabs: the preparation pipeline
//! produces all of this as one analysis, so it reads as one document.
//! - Hero: book synopsis (the "what") + style guide (the "how") side by side.
//! - Voice: 7 style fields in a compact grid, one Save for all.
//! - Cast & key terms: read-only tables from `characters`/`terms` analysis.
//! - Chapter digests: per-chapter edit/save/cancel list.
//! - readOnly = busy || saving || !data. Drafts are session-only view state.

use std::collections::HashMap;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Disableable, StyledExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::{
    client::{block_on, is_project_busy, AnalysisOut, Client},
    theme,
};

/// The 7 voice fields — mirrors `styleFields` in `StylePage.tsx`.
/// Grouped: narrative craft (genre/tone/narration) vs language surface
/// (pacing/register/dialogue/rhetoric) for a scannable two-row grid.
const VOICE_FIELDS: &[(&str, &str)] = &[
    ("genre", "Genre"),
    ("tone", "Tone"),
    ("narration", "Narration"),
    ("pacing", "Pacing"),
    ("register", "Register"),
    ("dialogue_style", "Dialogue style"),
    ("rhetoric", "Rhetoric"),
];

pub struct StyleView {
    client: Option<Client>,
    pid: String,
    project_status: String,
    data: Option<AnalysisOut>,
    loading: bool,
    error: Option<String>,
    inputs: HashMap<String, Entity<TextareaState>>,
    saving: bool,
    refreshing: bool,
    /// Per-digest edit state: ci -> draft.
    digest_drafts: HashMap<u32, String>,
    digest_inputs: HashMap<u32, Entity<TextareaState>>,
    editing_digest: Option<u32>,
    saving_digest: bool,
}

/// All editable analysis keys in one save unit.
const SAVE_KEYS: &[&str] = &[
    "genre",
    "tone",
    "narration",
    "pacing",
    "register",
    "dialogue_style",
    "rhetoric",
    "style_guide",
    "book_synopsis",
];

impl StyleView {
    pub fn new(pid: String) -> Self {
        Self {
            client: None,
            pid,
            project_status: String::new(),
            data: None,
            loading: true,
            error: None,
            inputs: HashMap::new(),
            saving: false,
            refreshing: false,
            digest_drafts: HashMap::new(),
            digest_inputs: HashMap::new(),
            editing_digest: None,
            saving_digest: false,
        }
    }

    pub fn set_client(&mut self, client: Client, cx: &mut Context<Self>) {
        self.client = Some(client);
        self.refresh(cx);
    }

    fn read_only(&self) -> bool {
        self.busy() || self.saving || self.data.is_none()
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
                    // Sequential, not join!: analysis may 500 before
                    // preparation produces it; status must still render.
                    let data = block_on(client.get_analysis(&pid));
                    let project = block_on(client.get_project(&pid))?;
                    Ok::<_, crate::client::Error>((data, project.project.status))
                })
                .await;
            _ = this.update(cx, |view: &mut StyleView, cx| {
                view.loading = false;
                match result {
                    Ok((data, status)) => {
                        match data {
                            Ok(data) => {
                                view.data = Some(data);
                                view.error = None;
                            }
                            Err(err) => {
                                view.data = Some(AnalysisOut {
                                    analysis: serde_json::Value::Object(Default::default()),
                                    chapter_digests: Vec::new(),
                                });
                                view.error = Some(format!("analysis: {err}"));
                            }
                        }
                        view.project_status = status;
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

    fn value_for(&self, key: &str) -> String {
        self.data
            .as_ref()
            .and_then(|d| d.analysis.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    }

    /// All editable keys in one save: voice fields + guide + synopsis.
    /// The old UI had three separate Save buttons; edits across sections
    /// are one analysis document, so they save as one unit.
    fn save_all(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let mut analysis = serde_json::Map::new();
        for key in SAVE_KEYS {
            let value = self
                .inputs
                .get(*key)
                .map(|input| input.read(cx).value().to_string())
                .unwrap_or_else(|| self.value_for(key));
            analysis.insert(key.to_string(), serde_json::Value::String(value));
        }
        let pid = self.pid.clone();
        self.saving = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    block_on(client.update_analysis(&pid, serde_json::Value::Object(analysis)))
                })
                .await;
            _ = this.update(cx, |view: &mut StyleView, cx| {
                view.saving = false;
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

    fn refresh_analysis(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let pid = self.pid.clone();
        self.refreshing = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { block_on(client.refresh_analysis(&pid)) })
                .await;
            _ = this.update(cx, |view: &mut StyleView, cx| {
                view.refreshing = false;
                match result {
                    // Job runs in background; poll until it lands.
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

    fn save_digest(&mut self, ci: u32, cx: &mut Context<Self>) {
        let (Some(client), Some(input)) =
            (self.client.clone(), self.digest_inputs.get(&ci).cloned())
        else {
            return;
        };
        let digest: String = input.read(cx).value().to_string();
        let pid = self.pid.clone();
        self.saving_digest = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { block_on(client.update_digest(&pid, ci, &digest)) })
                .await;
            _ = this.update(cx, |view: &mut StyleView, cx| {
                view.saving_digest = false;
                match result {
                    Ok(_) => {
                        view.editing_digest = None;
                        view.digest_drafts.remove(&ci);
                        view.digest_inputs.remove(&ci);
                        view.refresh(cx);
                    }
                    Err(err) => {
                        view.error = Some(err.to_string());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn field_input(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TextareaState> {
        if let Some(input) = self.inputs.get(key) {
            return input.clone();
        }
        let initial = self.value_for(key);
        let input = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx);
            state.set_value(initial, window, cx);
            state
        });
        self.inputs.insert(key.to_string(), input.clone());
        input
    }

    /// Read-only rows from a `[{source, target, ...}]` analysis list.
    fn cast_rows(list: &serde_json::Value) -> Vec<(String, String, String)> {
        list.as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let source = item.get("source")?.as_str()?.to_string();
                        let target = item.get("target")?.as_str().unwrap_or("").to_string();
                        let note = item.get("note")?.as_str().unwrap_or("").to_string();
                        (!source.is_empty()).then_some((source, target, note))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Render for StyleView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current_palette();
        let read_only = self.read_only();
        // Single scrollable overview: synopsis + guide hero, voice grid,
        // cast/terms tables, chapter digests. No tabs — one document.
        let mut col = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .max_w(px(1180.0))
            .gap_5()
            .p_6()
            .overflow_y_scrollbar()
            .font(theme::fonts::ui())
            .child(
                div().flex().items_center().justify_between()
                    .child(super::ui::page_title("Book knowledge", None::<Div>))
                    .child(
                        div().flex().items_center().gap_2()
                            .child(
                                Button::new("refresh-analysis")
                                    .ghost()
                                    .label(if self.refreshing { "Refreshing…" } else { "Refresh analysis" })
                                    .disabled(read_only || self.refreshing)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.refresh_analysis(cx);
                                    })),
                            )
                            .child(
                                Button::new("save-all")
                                    .primary()
                                    .label(if self.saving { "Saving…" } else { "Save changes" })
                                    .disabled(read_only || self.saving)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.save_all(cx);
                                    })),
                            ),
                    ),
            );

        if let Some(err) = &self.error {
            col = col.child(super::ui::error_box(err, &palette));
        }
        if self.loading && self.data.is_none() {
            col = col.child(div().text_sm().text_color(palette.muted_foreground).child("Loading…"));
            return col;
        }
        if read_only && self.busy() {
            col = col.child(
                div().text_sm().text_color(palette.muted_foreground)
                    .child("Project is running — knowledge is read-only."),
            );
        }

        // Hero: what the book is about + how to translate it, side by side.
        // Multi-line: both hold full paragraphs, not one-liners.
        let synopsis = self.field_input("book_synopsis", window, cx);
        let guide = self.field_input("style_guide", window, cx);
        col = col.child(
            div().flex().gap_3()
                .child(
                    div().flex_1().min_w_0().flex().flex_col().gap_1()
                        .p_3().rounded_sm().border_1().border_color(palette.border).bg(palette.card)
                        .child(div().text_sm().font_medium().child("Book synopsis"))
                        .child(
                            div().text_xs().text_color(palette.muted_foreground)
                                .child("Steers chapter context, review, and autofix."),
                        )
                        .child(Textarea::new(&synopsis).h(px(160.))),
                )
                .child(
                    div().flex_1().min_w_0().flex().flex_col().gap_1()
                        .p_3().rounded_sm().border_1().border_color(palette.border).bg(palette.card)
                        .child(div().text_sm().font_medium().child("Style guide"))
                        .child(
                            div().text_xs().text_color(palette.muted_foreground)
                                .child("Translator instructions: names, quotes, tone."),
                        )
                        .child(Textarea::new(&guide).h(px(160.))),
                ),
        );

        // Voice: 7 fields in a compact grid (3 + 3 + 1).
        col = col.child(div().text_sm().font_medium().child("Voice"));
        let mut grid = div().flex().flex_col().gap_3();
        let mut row: Option<Div> = None;
        for (i, (key, label)) in VOICE_FIELDS.iter().enumerate() {
            let input = self.field_input(key, window, cx);
            let card = div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .p_3()
                .rounded_sm()
                .border_1()
                .border_color(palette.border)
                .bg(palette.card)
                .child(div().text_sm().font_medium().child(label.to_string()))
                .child(Textarea::new(&input).h(px(96.)));
            // 3-column rows: flush every third card.
            if i % 3 == 0 {
                row = Some(div().flex().gap_3().child(card));
            } else if let Some(r) = row.take() {
                let r = r.child(card);
                if i % 3 == 2 {
                    grid = grid.child(r);
                } else {
                    row = Some(r);
                }
            }
        }
        if let Some(r) = row.take() {
            grid = grid.child(r);
        }
        col = col.child(grid);

        // Cast & key terms: read-only reference tables (managed by preparation).
        let empty = serde_json::Value::Null;
        let analysis = self.data.as_ref().map(|d| &d.analysis).unwrap_or(&empty);
        for (title, key, hint) in [
            ("Cast", "characters", "Who appears, how names translate."),
            ("Key terms", "terms", "Places, titles, fixed expressions."),
        ] {
            let rows = Self::cast_rows(analysis.get(key).unwrap_or(&empty));
            if rows.is_empty() {
                continue;
            }
            let mut table = div().flex().flex_col().rounded_sm()
                .border_1().border_color(palette.border).bg(palette.card);
            table = table.child(
                div().px_3().pt_3().pb_1()
                    .child(div().text_sm().font_medium().child(title))
                    .child(div().text_xs().text_color(palette.muted_foreground).child(hint)),
            );
            for (source, target, note) in rows.iter().take(30) {
                table = table.child(
                    div().flex().gap_3().px_3().py_1p5()
                        .border_t_1().border_color(palette.border)
                        .child(div().w_40().flex_shrink_0().text_sm().font_medium().child(source.clone()))
                        .child(div().w_40().flex_shrink_0().text_sm().child(target.clone()))
                        .child(div().flex_1().min_w_0().text_xs().text_color(palette.muted_foreground).child(note.clone())),
                );
            }
            if rows.len() > 30 {
                table = table.child(
                    div().px_3().py_1p5().text_xs().text_color(palette.muted_foreground)
                        .child(format!("+ {} more (see Glossary)", rows.len() - 30)),
                );
            }
            col = col.child(table);
        }

        // Chapter digests: per-chapter context summaries, editable in place.
        col = col.child(div().text_sm().font_medium().child("Chapter digests"));
        let digests = self
            .data
            .as_ref()
            .map(|d| d.chapter_digests.clone())
            .unwrap_or_default();
        if digests.is_empty() {
            col = col.child(
                div().text_sm().text_color(palette.muted_foreground)
                    .child("No chapter digests yet — preparation generates them."),
            );
        } else {
            let mut list = div().flex().flex_col().gap_2();
            for digest in digests {
                let ci = digest.index;
                if self.editing_digest == Some(ci) {
                    if !self.digest_inputs.contains_key(&ci) {
                        let initial = self
                            .digest_drafts
                            .get(&ci)
                            .cloned()
                            .unwrap_or_else(|| digest.digest.clone());
                        let input = cx.new(|cx| {
                            let mut state = TextareaState::new(window, cx);
                            state.set_value(initial, window, cx);
                            state
                        });
                        self.digest_inputs.insert(ci, input);
                    }
                    let input = self.digest_inputs[&ci].clone();
                    list = list.child(
                        div().flex().flex_col().gap_1().p_3().rounded_md()
                            .border_1().border_color(palette.ring)
                            .on_key_down(cx.listener(
                                move |this, ev: &gpui_kit::KeyDownEvent, _, cx| {
                                    if ev.keystroke.key == "escape" {
                                        this.editing_digest = None;
                                        this.digest_drafts.remove(&ci);
                                        this.digest_inputs.remove(&ci);
                                        cx.notify();
                                    }
                                },
                            ))
                            .child(div().font_medium().child(format!(
                                "#{ci} {}",
                                digest.title
                            )))
                            .child(Textarea::new(&input).h(px(128.)))
                            .child(
                                div().flex().gap_2()
                                    .child(
                                        Button::new(format!("save-digest-{ci}"))
                                            .primary()
                                            .label(if self.saving_digest { "Saving…" } else { "Save" })
                                            .disabled(self.saving_digest)
                                            .on_click(cx.listener(
                                                move |this, _, _, cx| {
                                                    this.save_digest(ci, cx);
                                                },
                                            )),
                                    )
                                    .child(
                                        Button::new(format!("cancel-digest-{ci}"))
                                            .ghost()
                                            .label("Cancel")
                                            .on_click(cx.listener(
                                                move |this, _, _, cx| {
                                                    this.editing_digest = None;
                                                    this.digest_drafts.remove(&ci);
                                                    this.digest_inputs.remove(&ci);
                                                    cx.notify();
                                                },
                                            )),
                                    ),
                            ),
                    );
                } else {
                    list = list.child(
                        div().flex().flex_col().gap_1().p_3().rounded_md()
                            .border_1().border_color(palette.border)
                            .child(
                                div().flex().items_center().justify_between()
                                    .child(div().font_medium().child(format!(
                                        "#{ci} {}",
                                        digest.title
                                    )))
                                    .child(
                                        Button::new(format!("edit-digest-{ci}"))
                                            .ghost()
                                            .label("Edit")
                                            .disabled(read_only)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.editing_digest = Some(ci);
                                                cx.notify();
                                            })),
                                    ),
                            )
                            .child(
                                if digest.digest.is_empty() {
                                    div().text_sm().text_color(palette.muted_foreground)
                                        .child("No digest yet.").into_any_element()
                                } else {
                                    div().text_sm().child(digest.digest.clone()).into_any_element()
                                }
                            ),
                    );
                }
            }
            col = col.child(list);
        }
        col
    }
}
