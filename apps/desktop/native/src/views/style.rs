//! Style & synopsis (screen 5): style fields + guide + synopsis + digests.
//!
//! Parity source: `features/style/StylePage.tsx`.
//! - Tabs: style / synopsis / digests.
//! - 7 style fields + save group; style guide textarea + save group;
//!   book synopsis textarea + save; per-chapter digest edit/save/cancel.
//! - readOnly = busy || saving || !data. Drafts are session-only (no browser
//!   storage) — mirrored as view state.

use std::collections::HashMap;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Disableable, StyledExt};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::{
    client::{block_on, is_project_busy, AnalysisOut, Client},
    theme,
};

/// The 7 style fields — mirrors `styleFields` in `StylePage.tsx`.
const STYLE_FIELDS: &[(&str, &str)] = &[
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
    tab: usize,
    /// Draft values for style fields + guide + synopsis.
    drafts: HashMap<String, String>,
    inputs: HashMap<String, Entity<InputState>>,
    saving_keys: Option<Vec<String>>,
    saving: bool,
    /// Per-digest edit state: ci -> draft.
    digest_drafts: HashMap<u32, String>,
    digest_inputs: HashMap<u32, Entity<InputState>>,
    editing_digest: Option<u32>,
    saving_digest: bool,
}

impl StyleView {
    pub fn new(pid: String) -> Self {
        Self {
            client: None,
            pid,
            project_status: String::new(),
            data: None,
            loading: true,
            error: None,
            tab: 0,
            drafts: HashMap::new(),
            inputs: HashMap::new(),
            saving_keys: None,
            saving: false,
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
                    let project = block_on(client.get_project(&pid))?;
                    // Analysis may 500 before preparation produces it — the
                    // source renders the form with empty values + ErrorNotice.
                    let data = block_on(client.get_analysis(&pid)).ok();
                    Ok::<_, crate::client::Error>((data, project.project.status))
                })
                .await;
            _ = this.update(cx, |view: &mut StyleView, cx| {
                view.loading = false;
                match result {
                    Ok((data, status)) => {
                        view.data = Some(data.unwrap_or(AnalysisOut {
                            analysis: serde_json::Value::Object(Default::default()),
                            chapter_digests: Vec::new(),
                        }));
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

    fn value_for(&self, key: &str) -> String {
        if let Some(draft) = self.drafts.get(key) {
            return draft.clone();
        }
        self.data
            .as_ref()
            .and_then(|d| d.analysis.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    }

    fn save_keys(&mut self, keys: Vec<String>, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        // Collect current input values for the keys being saved.
        let mut analysis = serde_json::Map::new();
        for key in &keys {
            let value = self
                .inputs
                .get(key)
                .map(|input| input.read(cx).value().to_string())
                .unwrap_or_else(|| self.value_for(key));
            analysis.insert(key.clone(), serde_json::Value::String(value));
        }
        let pid = self.pid.clone();
        self.saving = true;
        self.saving_keys = Some(keys);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    block_on(client.update_analysis(&pid, serde_json::Value::Object(analysis)))
                })
                .await;
            _ = this.update(cx, |view: &mut StyleView, cx| {
                view.saving = false;
                view.saving_keys = None;
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
    ) -> Entity<InputState> {
        if let Some(input) = self.inputs.get(key) {
            return input.clone();
        }
        let initial = self.value_for(key);
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(initial, window, cx);
            state
        });
        self.inputs.insert(key.to_string(), input.clone());
        input
    }
}

impl Render for StyleView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current_palette();
        let read_only = self.read_only();
        // Scroll region owns overflow; flex_1 + min_h_0 lets it shrink in
        // the app-shell column. Tab order: tab buttons → inputs → save.
        let mut col = div().flex().flex_col().flex_1().min_h_0().gap_3().p_6()
            .overflow_y_scrollbar().child(
            div().text_xl().font_weight(FontWeight::BOLD).child("Style & synopsis"),
        );

        if let Some(err) = &self.error {
            col = col.child(
                div().p_3().rounded_md().bg(palette.destructive)
                    .text_color(palette.destructive_foreground)
                    .child(format!("Error: {err}")),
            );
        }
        if self.loading && self.data.is_none() {
            col = col.child(div().text_sm().text_color(palette.muted_foreground).child("Loading…"));
            return col;
        }
        if read_only && self.busy() {
            col = col.child(
                div().text_sm().text_color(palette.muted_foreground)
                    .child("Project is running — style is read-only."),
            );
        }

        col = col.child(
            TabBar::new("style-tabs")
                .selected_index(self.tab)
                .child(Tab::new().label("Style"))
                .child(Tab::new().label("Synopsis"))
                .child(Tab::new().label("Digests")),
        );
        // Tab switching via buttons (TabBar callback pass lands later).
        col = col.child(
            div().flex().gap_1()
                .child(
                    Button::new("tab-style").ghost().label("Style").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.tab = 0;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    Button::new("tab-synopsis").ghost().label("Synopsis").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.tab = 1;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    Button::new("tab-digests").ghost().label("Digests").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.tab = 2;
                            cx.notify();
                        }),
                    ),
                ),
        );

        match self.tab {
            0 => {
                // Style fields + guide, each group with its own save.
                let mut form = div().flex().flex_col().gap_2();
                for (key, label) in STYLE_FIELDS {
                    let input = self.field_input(key, window, cx);
                    form = form.child(
                        div().flex().flex_col().gap_1()
                            .child(div().text_sm().font_medium().child(label.to_string()))
                            .child(Input::new(&input)),
                    );
                }
                let keys: Vec<String> =
                    STYLE_FIELDS.iter().map(|(k, _)| k.to_string()).collect();
                form = form.child(
                    Button::new("save-style")
                        .primary()
                        .label(if self.saving { "Saving…" } else { "Save style" })
                        .disabled(read_only || self.saving)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.save_keys(keys.clone(), cx);
                        })),
                );
                let guide = self.field_input("style_guide", window, cx);
                form = form.child(
                    div().flex().flex_col().gap_1()
                        .child(div().text_sm().font_medium().child("Style guide"))
                        .child(Input::new(&guide)),
                );
                form = form.child(
                    Button::new("save-guide")
                        .primary()
                        .label("Save guide")
                        .disabled(read_only || self.saving)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.save_keys(vec!["style_guide".into()], cx);
                        })),
                );
                col = col.child(form);
            }
            1 => {
                let synopsis = self.field_input("book_synopsis", window, cx);
                col = col.child(
                    div().flex().flex_col().gap_2()
                        .child(div().text_sm().font_medium().child("Book synopsis"))
                        .child(Input::new(&synopsis))
                        .child(
                            Button::new("save-synopsis")
                                .primary()
                                .label("Save synopsis")
                                .disabled(read_only || self.saving)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.save_keys(vec!["book_synopsis".into()], cx);
                                })),
                        ),
                );
            }
            _ => {
                // Chapter digests list with per-digest edit/save/cancel.
                let digests = self
                    .data
                    .as_ref()
                    .map(|d| d.chapter_digests.clone())
                    .unwrap_or_default();
                if digests.is_empty() {
                    col = col.child(
                        div().text_sm().text_color(palette.muted_foreground)
                            .child("No chapter digests yet."),
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
                                    let mut state = InputState::new(window, cx);
                                    state.set_value(initial, window, cx);
                                    state
                                });
                                self.digest_inputs.insert(ci, input);
                            }
                            // Borrow ends before the on_click closures.
                            let input = self.digest_inputs[&ci].clone();
                            list = list.child(
                                div().flex().flex_col().gap_1().p_3().rounded_md()
                                    .border_1().border_color(palette.ring)
                                    .on_key_down(cx.listener(
                                        move |this, ev: &gpui_kit::KeyDownEvent, _, cx| {
                                            // Esc cancels the digest edit.
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
                                    .child(Input::new(&input))
                                    .child(
                                        div().flex().gap_2()
                                            .child(
                                                Button::new(format!("save-digest-{ci}"))
                                                    .primary()
                                                    .label(if self.saving_digest {
                                                        "Saving…"
                                                    } else {
                                                        "Save"
                                                    })
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
                                    .child(div().font_medium().child(format!(
                                        "#{ci} {}",
                                        digest.title
                                    )))
                                    .child(div().text_sm().child(digest.digest.clone()))
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
                            );
                        }
                    }
                    col = col.child(list);
                }
            }
        }
        col
    }
}
