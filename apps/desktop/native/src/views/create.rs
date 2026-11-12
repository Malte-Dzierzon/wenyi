//! Create project (screen 2): form + file browse + create.
//!
//! Parity source: `features/project-create/CreateProject.tsx`,
//! `SourceFilePicker.tsx`.
//! - Fields: name, source/target lang, PDF backend (pdf only), mode radios,
//!   prepare checkbox. Resume-edit `?project=` path deferred to U1 follow-up
//!   (needs project deeplink state; documented gap).
//! - Browse via `rfd` (off UI thread); validation mirrors picker (nonempty +
//!   extension allowlist from capabilities).
//! - Create: multipart `POST /projects`.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::radio::{Radio, RadioGroup};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::{Disableable, IndexPath};
use gpui_kit::*;

use crate::{
    service::{Capabilities, ServiceHandle},
    theme,
};

/// Allowed source extensions — mirrors the desktop allowlist
/// (`native_drop.rs`) intersected with `capabilities.input_formats`.
fn allowed_extensions(caps: Option<&Capabilities>) -> Vec<String> {
    let base = [
        "epub", "fb2", "txt", "text", "md", "markdown", "html", "htm", "pdf", "docx", "srt",
    ];
    match caps {
        None => base.iter().map(|s| s.to_string()).collect(),
        Some(caps) => base
            .iter()
            .filter(|e| caps.input_formats.iter().any(|f| f == *e))
            .map(|s| s.to_string())
            .collect(),
    }
}

pub struct Create {
    service: Option<ServiceHandle>,
    capabilities: Option<Capabilities>,
    name: Entity<InputState>,
    source_lang: String,
    target_lang: String,
    translation_mode: String,
    prepare: bool,
    pdf_backend: Option<String>,
    file_path: Option<std::path::PathBuf>,
    /// Name prefill applied on next render (set_value needs Window).
    pending_name: Option<String>,
    file_error: Option<String>,
    created_pid: Option<String>,
    busy: bool,
    error: Option<String>,
    source_select: Option<Entity<SelectState<Vec<String>>>>,
    target_select: Option<Entity<SelectState<Vec<String>>>>,
}

impl Create {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Project name"));
        Self {
            service: None,
            capabilities: None,
            name,
            source_lang: "auto".into(),
            target_lang: "zh".into(),
            translation_mode: "standard".into(),
            prepare: false,
            pdf_backend: None,
            file_path: None,
            pending_name: None,
            file_error: None,
            created_pid: None,
            busy: false,
            error: None,
            source_select: None,
            target_select: None,
        }
    }
    // Wired by the App shell once the create screen joins the nav graph.
    #[allow(dead_code)]
    pub fn set_client(&mut self, service: ServiceHandle, cx: &mut Context<Self>) {
        self.service = Some(service.clone());
        // Load capabilities for language lists + format allowlist.
        cx.spawn(async move |this, cx| {
            let caps = cx
                .background_spawn(async move { service.capabilities() })
                .await;
            _ = this.update(cx, |view: &mut Create, cx| {
                view.capabilities = Some(caps);
                cx.notify();
            });
        })
        .detach();
    }

    fn browse(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let extensions = allowed_extensions(self.capabilities.as_ref());
        cx.spawn(async move |this, cx| {
            let picked = cx
                .background_spawn(async move {
                    rfd::FileDialog::new()
                        .add_filter(
                            "source",
                            &extensions.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                        )
                        .pick_file()
                })
                .await;
            _ = this.update(cx, |view: &mut Create, cx| {
                if let Some(path) = picked {
                    // Validate: nonempty + extension allowlist (mirrors picker).
                    let ext_ok = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .map(|e| {
                            allowed_extensions(view.capabilities.as_ref())
                                .iter()
                                .any(|a| a == &e.to_lowercase())
                        })
                        .unwrap_or(false);
                    let size_ok = std::fs::metadata(&path)
                        .map(|m| m.len() > 0)
                        .unwrap_or(false);
                    if !ext_ok {
                        view.file_error = Some("Unsupported file type".into());
                    } else if !size_ok {
                        view.file_error = Some("Source file is empty".into());
                    } else {
                        view.file_error = None;
                        // PDF backend select appears for pdf (mirrors source).
                        if path
                            .extension()
                            .and_then(|e| e.to_str())
                            .map(|e| e.eq_ignore_ascii_case("pdf"))
                            .unwrap_or(false)
                        {
                            view.pdf_backend = Some("mineru".into());
                        } else {
                            view.pdf_backend = None;
                        }
                        // Default the project name from the file stem.
                        // Applied on next render (set_value needs Window).
                        if view.name.read(cx).value().is_empty() {
                            view.pending_name = path
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .map(str::to_string);
                        }
                        view.file_path = Some(path);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(service), Some(path)) = (self.service.clone(), self.file_path.clone()) else {
            self.error = Some("Pick a source file first".into());
            cx.notify();
            return;
        };
        let name: String = self.name.read(cx).value().to_string();
        let name = name.trim().to_string();
        if name.is_empty() {
            self.error = Some("Project name is required".into());
            cx.notify();
            return;
        }
        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("source")
            .to_string();
        let source_lang = self.source_lang.clone();
        let target_lang = self.target_lang.clone();
        self.busy = true;
        self.error = None;
        cx.notify();
        // Drop window borrow: the async block owns everything it needs.
        let _ = window;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    // Read on the background thread: a multi-MB source file
                    // must not block the UI thread.
                    let bytes = match std::fs::read(&path) {
                        Ok(bytes) if !bytes.is_empty() => bytes,
                        _ => return Err("Could not read source file".to_string()),
                    };
                    service
                        .create_project(&name, &source_lang, &target_lang, &bytes, &filename)
                        .map_err(|err| err.to_string())
                })
                .await;
            _ = this.update(cx, |view: &mut Create, cx| {
                view.busy = false;
                match result {
                    Ok(detail) => {
                        view.created_pid = Some(detail.project.id.clone());
                    }
                    Err(err) => {
                        view.error = Some(err);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn lang_options(&self) -> Vec<String> {
        let mut opts = vec!["auto".to_string()];
        if let Some(caps) = &self.capabilities {
            opts.extend(caps.languages.iter().map(|l| l.code.clone()));
        } else {
            // Fallback until capabilities load (mirrors registry subset).
            opts.extend(
                [
                    "zh", "en", "ja", "ko", "fr", "de", "es", "it", "pt", "ru", "vi",
                ]
                .iter()
                .map(|s| s.to_string()),
            );
        }
        opts
    }
}

impl Render for Create {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Apply deferred name prefill (needs Window for set_value).
        if let Some(stem) = self.pending_name.take() {
            if self.name.read(cx).value().is_empty() {
                self.name.update(cx, |state, cx| {
                    state.set_value(stem, window, cx);
                });
            }
        }
        let palette = theme::current_palette();
        // Lazily build language selects once (entity state persists),
        // subscribing to Confirm so selection writes back to the form state.
        if self.source_select.is_none() {
            let opts = self.lang_options();
            let idx = opts
                .iter()
                .position(|o| o == &self.source_lang)
                .unwrap_or(0);
            let entity = cx.new(|cx| SelectState::new(opts, Some(IndexPath::new(idx)), window, cx));
            cx.subscribe(
                &entity,
                |this: &mut Create, _, event: &SelectEvent<Vec<String>>, _| {
                    if let SelectEvent::Confirm(Some(value)) = event {
                        this.source_lang = value.clone();
                    }
                },
            )
            .detach();
            self.source_select = Some(entity);
        }
        if self.target_select.is_none() {
            let opts = self.lang_options();
            let idx = opts
                .iter()
                .position(|o| o == &self.target_lang)
                .unwrap_or(1);
            let entity = cx.new(|cx| SelectState::new(opts, Some(IndexPath::new(idx)), window, cx));
            cx.subscribe(
                &entity,
                |this: &mut Create, _, event: &SelectEvent<Vec<String>>, _| {
                    if let SelectEvent::Confirm(Some(value)) = event {
                        this.target_lang = value.clone();
                    }
                },
            )
            .detach();
            self.target_select = Some(entity);
        }
        let mode_standard = self.translation_mode == "standard";
        // Scroll region owns overflow; flex_1 + min_h_0 lets it shrink in
        // the app-shell column.
        let mut col = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .w_full()
            .max_w(px(820.0))
            .gap_4()
            .p_6()
            .overflow_y_scrollbar()
            .font(theme::fonts::ui())
            .child(super::ui::page_title("Create project", None::<Div>));

        if let Some(err) = &self.error {
            col = col.child(super::ui::error_box(err, &palette));
        }

        col = col
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child("Project name")
                    .child(Input::new(&self.name)),
            )
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child("Source language")
                            .child(Select::new(self.source_select.as_ref().unwrap())),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child("Target language")
                            .child(Select::new(self.target_select.as_ref().unwrap())),
                    ),
            )
            .child(
                RadioGroup::new("mode")
                    .child(
                        Radio::new("mode-standard")
                            .label("Standard")
                            .checked(mode_standard),
                    )
                    .child(
                        Radio::new("mode-best")
                            .label("Best of three (precision)")
                            .checked(!mode_standard),
                    )
                    .on_change(cx.listener(|this, index: &usize, _, cx| {
                        this.translation_mode = if *index == 0 {
                            "standard"
                        } else {
                            "best_of_three"
                        }
                        .into();
                        cx.notify();
                    })),
            )
            .child(
                Checkbox::new("prepare")
                    .label("Prepare (parse + analyze before translating)")
                    .checked(self.prepare)
                    .on_change(cx.listener(|this, checked: &bool, _, cx| {
                        this.prepare = *checked;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("browse")
                            .label("Browse source file…")
                            .disabled(self.busy)
                            .on_click(cx.listener(|this, _, window, cx| this.browse(window, cx))),
                    )
                    .child(match &self.file_path {
                        Some(p) => p.display().to_string(),
                        None => "No file selected".to_string(),
                    }),
            );

        if let Some(err) = &self.file_error {
            col = col.child(
                div()
                    .text_sm()
                    .text_color(palette.destructive)
                    .child(err.clone()),
            );
        }

        col = col.child(
            Button::new("create")
                .primary()
                .label(if self.busy {
                    "Creating…"
                } else {
                    "Create project"
                })
                .disabled(self.busy || self.file_path.is_none())
                .on_click(cx.listener(|this, _, window, cx| this.create(window, cx))),
        );

        if let Some(pid) = &self.created_pid {
            col = col.child(
                div()
                    .text_sm()
                    .text_color(palette.muted_foreground)
                    .child(format!(
                        "Created project {pid}. Open it from Projects to continue."
                    )),
            );
        }
        col
    }
}
