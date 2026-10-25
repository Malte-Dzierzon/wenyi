//! Export (screen 7): format/edition options + generate + history + native save.
//!
//! Parity source: `features/export/ExportPage.tsx`.
//! - Queries on load: `get_config` (defaults for bilingual/punctuation/about/
//!   preserve/order), `get_project` (subtitle lock), `capabilities` (format
//!   grid + PDF export backends), `list_exports` (2s poll while pending,
//!   running, or queued).
//! - Format grid: Automatic + `caps.output_formats` (minus `srt` unless the
//!   project itself is a subtitle project, which forces `srt` + `about=false`).
//! - Monolingual/bilingual radios; advanced disclosure (punctuation checkbox,
//!   PDF engine select when `fmt == pdf`, bilingual order select, about-page
//!   checkbox, preserve-style checkbox for bilingual output).
//! - Generate: disabled while generating, uninitialized (no project fmt),
//!   no caps, defaults not loaded, or config error. Desktop ALWAYS uses the
//!   native path: `plan_native_export` → `rfd` save dialog → `create_export`
//!   → poll until done → `native_export_content` → write file. Per-row
//!   "Save as" reuses `GET …/plan` + `…/content` for that export id.
//! - States: loading / error (aggregated notice) / empty table row / busy and
//!   read-only gating mirrors the source (no project-busy gate in source).

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::radio::{Radio, RadioGroup};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Disableable, IndexPath, StyledExt};
use gpui_kit::*;

use crate::{
    client::{block_on, Client, ExportOut, ExportRequest, NativeExportPlan},
    theme,
};

/// Active export statuses — mirrors the `refetchInterval` guard in the source
/// (`pending`, `running`, `queued`).
fn is_export_active(status: &str) -> bool {
    matches!(status, "pending" | "running" | "queued")
}

fn format_bytes(size: Option<i64>) -> String {
    match size {
        None => "—".into(),
        Some(n) if n < 0 => "—".into(),
        Some(n) => {
            const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
            let mut v = n as f64;
            let mut u = 0;
            while v >= 1024.0 && u + 1 < UNITS.len() {
                v /= 1024.0;
                u += 1;
            }
            if u == 0 {
                format!("{n} B")
            } else {
                format!("{v:.1} {}", UNITS[u])
            }
        }
    }
}

pub struct ExportView {
    client: Option<Client>,
    pid: String,
    project_fmt: Option<String>,
    caps_loaded: bool,
    caps_formats: Vec<String>,
    pdf_backends: Vec<String>,
    defaults_loaded: bool,
    project_error: Option<String>,
    config_error: Option<String>,
    caps_error: Option<String>,
    exports_error: Option<String>,
    op_error: Option<String>,
    format: String,
    bilingual: bool,
    order: String,
    about: bool,
    punctuation: bool,
    preserve_style: bool,
    pdf_backend: String,
    show_advanced: bool,
    exports: Option<Vec<ExportOut>>,
    generating: bool,
    saving_id: Option<i64>,
    notice: Option<String>,
    order_select: Option<Entity<SelectState<Vec<String>>>>,
    pdf_select: Option<Entity<SelectState<Vec<String>>>>,
    pdf_select_options: Vec<String>,
    /// Whether this screen is currently shown; the 2s active-export poll
    /// only re-arms while visible (set by the App shell on navigation).
    visible: bool,
}

impl ExportView {
    pub fn new(pid: String, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            client: None,
            pid,
            project_fmt: None,
            caps_loaded: false,
            caps_formats: Vec::new(),
            pdf_backends: Vec::new(),
            defaults_loaded: false,
            project_error: None,
            config_error: None,
            caps_error: None,
            exports_error: None,
            op_error: None,
            format: String::new(),
            bilingual: false,
            order: "target_first".into(),
            about: true,
            punctuation: true,
            preserve_style: false,
            pdf_backend: String::new(),
            show_advanced: false,
            exports: None,
            generating: false,
            saving_id: None,
            notice: None,
            order_select: None,
            pdf_select: None,
            pdf_select_options: Vec::new(),
            visible: true,
        }
    }

    /// Called by the App shell on navigation. Becoming visible resumes the
    /// active-export poll with an immediate refresh when exports are active.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if visible && !self.visible {
            self.visible = true;
            self.refresh_exports(cx);
        } else {
            self.visible = visible;
        }
    }

    pub fn set_client(&mut self, client: Client, cx: &mut Context<Self>) {
        self.client = Some(client);
        self.refresh_all(cx);
    }

    /// Initial load: config + project + capabilities + exports in one
    /// background round-trip (mirrors the four `useQuery` calls).
    fn refresh_all(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let pid = self.pid.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let config = block_on(client.get_config(&pid));
                    let project = block_on(client.get_project(&pid));
                    let caps = block_on(client.capabilities());
                    let caps_raw =
                        block_on(client.get::<serde_json::Value>("/capabilities"));
                    let exports = block_on(client.list_exports(&pid));
                    (config, project, caps, caps_raw, exports)
                })
                .await;
            _ = this.update(cx, |view: &mut ExportView, cx| {
                let (config, project, caps, caps_raw, exports) = result;
                match config {
                    Ok(cfg) => {
                        let output = cfg.effective.get("output").cloned().unwrap_or_default();
                        view.bilingual =
                            output.get("bilingual").and_then(|v| v.as_bool()).unwrap_or(false);
                        view.punctuation = output
                            .get("punctuation_normalize")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(true);
                        view.about = output
                            .get("about_page")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(true);
                        view.preserve_style = output
                            .get("bilingual_preserve_source_style")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        view.order = output
                            .get("bilingual_order")
                            .and_then(|v| v.as_str())
                            .filter(|s| *s == "source_first")
                            .unwrap_or("target_first")
                            .to_string();
                        view.config_error = None;
                        view.defaults_loaded = true;
                    }
                    Err(err) => {
                        view.config_error = Some(err.to_string());
                        view.defaults_loaded = false;
                    }
                }
                match project {
                    Ok(detail) => {
                        view.project_fmt = detail.project.fmt.clone();
                        view.project_error = None;
                    }
                    Err(err) => {
                        view.project_error = Some(err.to_string());
                        view.project_fmt = None;
                    }
                }
                match caps {
                    Ok(caps) => {
                        view.caps_formats = caps.output_formats;
                        view.caps_loaded = true;
                        view.caps_error = None;
                    }
                    Err(err) => {
                        view.caps_error = Some(err.to_string());
                        view.caps_loaded = false;
                    }
                }
                view.pdf_backends = caps_raw
                    .ok()
                    .and_then(|v| {
                        v.get("pdf")?.get("export_backends")?.as_array().cloned()
                    })
                    .map(|arr| {
                        arr.iter().filter_map(|b| b.as_str().map(|s| s.to_string())).collect()
                    })
                    .unwrap_or_default();
                match exports {
                    Ok(list) => {
                        view.exports_error = None;
                        view.exports = Some(list);
                    }
                    Err(err) => {
                        view.exports_error = Some(err.to_string());
                    }
                }
                cx.notify();
                view.schedule_poll(cx);
            });
        })
        .detach();
    }

    /// 2s refetch while any export is pending/running/queued (mirrors
    /// `refetchInterval` in the source).
    fn schedule_poll(&mut self, cx: &mut Context<Self>) {
        if !self.visible {
            return;
        }
        let active = self
            .exports
            .as_ref()
            .map(|list| list.iter().any(|e| is_export_active(&e.status)))
            .unwrap_or(false);
        if !active {
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(2))
                .await;
            _ = this.update(cx, |view: &mut ExportView, cx| {
                if view.visible {
                    view.refresh_exports(cx);
                }
            });
        })
        .detach();
    }

    fn refresh_exports(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let pid = self.pid.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { block_on(client.list_exports(&pid)) })
                .await;
            _ = this.update(cx, |view: &mut ExportView, cx| {
                match result {
                    Ok(list) => {
                        if view.exports.as_ref() != Some(&list) || view.exports_error.is_some() {
                            view.exports_error = None;
                            view.exports = Some(list);
                            cx.notify();
                        }
                    }
                    Err(err) => {
                        let msg = err.to_string();
                        if view.exports_error.as_ref() != Some(&msg) {
                            view.exports_error = Some(msg);
                            cx.notify();
                        }
                    }
                }
                view.schedule_poll(cx);
            });
        })
        .detach();
    }

    fn subtitle(&self) -> bool {
        self.project_fmt.as_deref() == Some("srt")
    }

    /// Effective format: subtitle projects force `srt`; otherwise the grid
    /// selection (`""` = Automatic).
    fn effective_format(&self) -> String {
        if self.subtitle() {
            "srt".into()
        } else {
            self.format.clone()
        }
    }

    fn format_options(&self) -> Vec<String> {
        if self.subtitle() {
            return vec!["srt".into()];
        }
        self.caps_formats.iter().filter(|f| *f != "srt").cloned().collect()
    }

    /// `ExportRequest::new()` defaults, then UI state (mirrors `create`).
    fn build_request(&self) -> ExportRequest {
        let mut req = ExportRequest::new();
        let fmt = self.effective_format();
        req.format = if fmt.is_empty() { None } else { Some(fmt.clone()) };
        req.bilingual = self.bilingual;
        req.order = self.order.clone();
        req.about_page = if self.subtitle() { false } else { self.about };
        req.preserve_source_style = self.preserve_style;
        req.punctuation_normalize = Some(self.punctuation);
        if fmt == "pdf" && !self.pdf_backend.is_empty() {
            req.pdf_engine = self.pdf_backend.clone();
        }
        req
    }

    fn generate_disabled(&self) -> bool {
        self.generating
            || self.project_fmt.as_deref().unwrap_or("").is_empty()
            || !self.caps_loaded
            || !self.defaults_loaded
            || self.config_error.is_some()
    }

    /// Generate via the desktop-native path: plan → save dialog → create →
    /// poll until done → content → write file. The whole blocking flow runs
    /// on a background thread; the UI only flips `generating`.
    fn generate(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if self.generate_disabled() {
            return;
        }
        let pid = self.pid.clone();
        let request = self.build_request();
        self.generating = true;
        self.op_error = None;
        self.notice = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let saved: Result<String, String> = cx
                .background_spawn(async move {
                    // 1. Plan the native filename (side-effect free).
                    let plan: NativeExportPlan = block_on(client.plan_native_export(&pid, &request))
                        .map_err(|e| e.to_string())?;
                    // 2. Destination dialog (blocking is fine off the UI thread).
                    let mut dialog = rfd::FileDialog::new()
                        .set_title(format!("Save {}", plan.label))
                        .set_file_name(&plan.filename);
                    if !plan.extension.is_empty() {
                        dialog = dialog.add_filter(&plan.label, &[plan.extension.as_str()]);
                    }
                    let Some(dest) = dialog.save_file() else {
                        return Err("__cancelled__".into());
                    };
                    // 3. Enqueue the export snapshot.
                    let created: serde_json::Value =
                        block_on(client.create_export(&pid, &request))
                            .map_err(|e| format!("{e} Export creation may have succeeded. Check export history before generating again."))?;
                    let export_id = created
                        .get("export_id")
                        .and_then(|v| v.as_i64())
                        .ok_or_else(|| {
                            "Invalid export job. Check export history before generating again."
                                .to_string()
                        })?;
                    // 4. Wait for completion (mirrors the Tauri task's poll).
                    loop {
                        let list = block_on(client.list_exports(&pid)).map_err(|e| e.to_string())?;
                        match list.iter().find(|e| e.id == export_id).map(|e| e.status.as_str()) {
                            Some("done") => break,
                            Some("pending") | Some("queued") | Some("running") => {
                                std::thread::sleep(std::time::Duration::from_millis(1000));
                            }
                            _ => {
                                return Err(format!(
                                    "Export generation failed (export {export_id}). Check export history."
                                ));
                            }
                        }
                    }
                    // 5. Fetch bytes + write (mirror `save_created_export`).
                    let bytes = block_on(client.native_export_content(&pid, export_id))
                        .map_err(|e| {
                            format!("{e} Export ID {export_id}: check export history and use Save as to recover this export; Generate creates a new snapshot.")
                        })?;
                    let mut dest = dest;
                    if !plan.extension.is_empty()
                        && dest.extension().and_then(|e| e.to_str()) != Some(plan.extension.as_str())
                    {
                        dest.set_extension(&plan.extension);
                    }
                    std::fs::write(&dest, &bytes).map_err(|e| e.to_string())?;
                    Ok(dest.to_string_lossy().into_owned())
                })
                .await;
            _ = this.update(cx, |view: &mut ExportView, cx| {
                view.generating = false;
                match saved {
                    Ok(path) => {
                        view.notice = Some(format!("Saved to {path}"));
                        view.op_error = None;
                    }
                    Err(cancelled) if cancelled == "__cancelled__" => {
                        view.op_error = None;
                    }
                    Err(err) => {
                        view.op_error = Some(err);
                    }
                }
                view.refresh_exports(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Per-row "Save as": plan for the existing export id → dialog →
    /// content → write file.
    fn save_row(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if self.saving_id.is_some() {
            return;
        }
        let pid = self.pid.clone();
        self.saving_id = Some(id);
        self.op_error = None;
        self.notice = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let saved: Result<String, String> = cx
                .background_spawn(async move {
                    let plan: NativeExportPlan = block_on(
                        client.get(&format!("/desktop/projects/{pid}/exports/{id}/plan")),
                    )
                    .map_err(|e| e.to_string())?;
                    let mut dialog = rfd::FileDialog::new()
                        .set_title(format!("Save {}", plan.label))
                        .set_file_name(&plan.filename);
                    if !plan.extension.is_empty() {
                        dialog = dialog.add_filter(&plan.label, &[plan.extension.as_str()]);
                    }
                    let Some(dest) = dialog.save_file() else {
                        return Err("__cancelled__".into());
                    };
                    let bytes = block_on(client.native_export_content(&pid, id))
                        .map_err(|e| e.to_string())?;
                    let mut dest = dest;
                    if !plan.extension.is_empty()
                        && dest.extension().and_then(|e| e.to_str()) != Some(plan.extension.as_str())
                    {
                        dest.set_extension(&plan.extension);
                    }
                    std::fs::write(&dest, &bytes).map_err(|e| e.to_string())?;
                    Ok(dest.to_string_lossy().into_owned())
                })
                .await;
            _ = this.update(cx, |view: &mut ExportView, cx| {
                view.saving_id = None;
                match saved {
                    Ok(path) => {
                        view.notice = Some(format!("Saved to {path}"));
                        view.op_error = None;
                    }
                    Err(cancelled) if cancelled == "__cancelled__" => {}
                    Err(err) => {
                        view.op_error = Some(err);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn error_text(&self) -> Option<String> {
        let mut parts = Vec::new();
        for e in [
            &self.project_error,
            &self.config_error,
            &self.caps_error,
            &self.exports_error,
            &self.op_error,
        ] {
            if let Some(e) = e {
                parts.push(e.clone());
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n"))
        }
    }
}

impl Render for ExportView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::current_palette();

        // Lazily build selects (entity state persists; Confirm writes back).
        if self.order_select.is_none() {
            let opts = vec!["Translation first".to_string(), "Source first".to_string()];
            let idx = if self.order == "source_first" { 1 } else { 0 };
            let entity = cx.new(|cx| {
                SelectState::new(opts, Some(IndexPath::new(idx)), window, cx)
            });
            cx.subscribe(&entity, |this: &mut ExportView, _, event: &SelectEvent<Vec<String>>, _| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    this.order = if value == "Source first" {
                        "source_first"
                    } else {
                        "target_first"
                    }
                    .into();
                }
            })
            .detach();
            self.order_select = Some(entity);
        }
        let pdf_opts: Vec<String> = std::iter::once("Automatic".to_string())
            .chain(self.pdf_backends.iter().cloned())
            .collect();
        if self.pdf_select.is_none() || self.pdf_select_options != pdf_opts {
            let idx = if self.pdf_backend.is_empty() {
                0
            } else {
                pdf_opts.iter().position(|o| o == &self.pdf_backend).unwrap_or(0)
            };
            let opts = pdf_opts.clone();
            let entity = cx.new(|cx| {
                SelectState::new(opts, Some(IndexPath::new(idx)), window, cx)
            });
            let backends = self.pdf_backends.clone();
            cx.subscribe(&entity, move |this: &mut ExportView, _, event: &SelectEvent<Vec<String>>, _| {
                if let SelectEvent::Confirm(Some(value)) = event {
                    this.pdf_backend = if value == "Automatic" || !backends.iter().any(|b| b == value) {
                        String::new()
                    } else {
                        value.clone()
                    };
                }
            })
            .detach();
            self.pdf_select = Some(entity);
            self.pdf_select_options = pdf_opts;
        }

        let subtitle = self.subtitle();
        let formats = self.format_options();
        let fmt = self.effective_format();
        let show_pdf_engine = fmt == "pdf" && !subtitle;

        // Scroll region owns overflow; flex_1 + min_h_0 lets it shrink in
        // the app-shell column.
        let mut col = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap_3()
            .p_6()
            .overflow_y_scrollbar()
            .child(div().text_xl().font_weight(FontWeight::BOLD).child("Export translation"));

        if let Some(err) = self.error_text() {
            col = col.child(
                div()
                    .p_3()
                    .rounded_md()
                    .bg(palette.destructive)
                    .text_color(palette.destructive_foreground)
                    .child(format!("Error: {err}")),
            );
        }
        if let Some(notice) = &self.notice {
            col = col.child(
                div()
                    .p_3()
                    .rounded_md()
                    .bg(palette.secondary)
                    .text_color(palette.secondary_foreground)
                    .child(notice.clone()),
            );
        }

        // Options card.
        let mut card = div()
            .flex()
            .flex_col()
            .gap_3()
            .p_5()
            .rounded_md()
            .border_1()
            .border_color(palette.border)
            .child(div().font_medium().child("Output format"));

        // Format grid (aria-pressed → primary/highlighted selection).
        {
            let mut grid = div().flex().flex_wrap().gap_2();
            if !subtitle {
                let selected = fmt.is_empty();
                let mut btn = Button::new("fmt-auto").label("Automatic");
                if selected {
                    btn = btn.primary();
                }
                grid = grid.child(btn.on_click(cx.listener(|this, _, _, cx| {
                    this.format.clear();
                    cx.notify();
                })));
            }
            for f in &formats {
                let f = f.clone();
                let selected = fmt == f;
                let mut btn = Button::new(format!("fmt-{f}")).label(f.to_uppercase());
                if selected {
                    btn = btn.primary();
                }
                grid = grid.child(btn.on_click(cx.listener(move |this, _, _, cx| {
                    this.format = f.clone();
                    cx.notify();
                })));
            }
            card = card.child(grid);
        }

        // Edition radios.
        {
            let bilingual = self.bilingual;
            card = card.child(
                div().flex().flex_col().gap_1().child("Edition").child(
                    RadioGroup::new("edition")
                        .child(
                            Radio::new("edition-mono")
                                .label("Monolingual")
                                .checked(!bilingual),
                        )
                        .child(
                            Radio::new("edition-bi")
                                .label("Bilingual")
                                .checked(bilingual),
                        )
                        .on_change(cx.listener(|this, index: &usize, _, cx| {
                            this.bilingual = *index == 1;
                            cx.notify();
                        })),
                ),
            );
        }

        // Advanced disclosure (native toggle; no Disclosure primitive in kit).
        if !subtitle {
            let expanded = self.show_advanced;
            card = card.child(
                Button::new("advanced-toggle")
                    .label(if expanded {
                        "Layout and advanced options ▾"
                    } else {
                        "Layout and advanced options ▸"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_advanced = !this.show_advanced;
                        cx.notify();
                    })),
            );
        }
        if self.show_advanced && !subtitle {
            let mut adv = div().flex().flex_col().gap_2();
            adv = adv.child(
                Checkbox::new("punctuation")
                    .label("Normalize punctuation on export")
                    .checked(self.punctuation)
                    .on_change(cx.listener(|this, checked: &bool, _, cx| {
                        this.punctuation = *checked;
                        cx.notify();
                    })),
            );
            if show_pdf_engine {
                adv = adv.child(
                    div().flex().flex_col().gap_1().child("PDF export engine").child(
                        Select::new(self.pdf_select.as_ref().unwrap()),
                    ),
                );
            }
            if self.bilingual {
                adv = adv.child(
                    div().flex().flex_col().gap_1().child("Bilingual order").child(
                        Select::new(self.order_select.as_ref().unwrap()),
                    ),
                );
            }
            adv = adv.child(
                Checkbox::new("about")
                    .label("Include an “About this translation” page")
                    .checked(self.about)
                    .on_change(cx.listener(|this, checked: &bool, _, cx| {
                        this.about = *checked;
                        cx.notify();
                    })),
            );
            if self.bilingual {
                adv = adv.child(
                    Checkbox::new("preserve-style")
                        .label("Preserve source formatting in bilingual output")
                        .checked(self.preserve_style)
                        .on_change(cx.listener(|this, checked: &bool, _, cx| {
                            this.preserve_style = *checked;
                            cx.notify();
                        })),
                );
            }
            card = card.child(adv);
        }

        let generating = self.generating;
        card = card.child(
            Button::new("generate")
                .primary()
                .label(if generating {
                    "Submitting…"
                } else {
                    "Generate export"
                })
                .disabled(self.generate_disabled())
                .on_click(cx.listener(|this, _, _, cx| this.generate(cx))),
        );
        col = col.child(card);

        // History card.
        let mut history = div()
            .flex()
            .flex_col()
            .gap_2()
            .p_5()
            .rounded_md()
            .border_1()
            .border_color(palette.border)
            .child(
                div()
                    .text_sm()
                    .text_color(palette.muted_foreground)
                    .child("Only the latest 5 exports are kept. Save files elsewhere to keep them."),
            );

        let header = div()
            .flex()
            .gap_3()
            .text_xs()
            .text_color(palette.muted_foreground)
            .child(div().w_24().child("Format"))
            .child(div().flex_1().child("Created"))
            .child(div().w_20().child("Size"))
            .child(div().flex_1().child("Status"))
            .child(div().w_24().child("Actions"));
        history = history.child(header);

        match &self.exports {
            None if self.exports_error.is_none() => {
                history = history.child(
                    div()
                        .text_sm()
                        .text_color(palette.muted_foreground)
                        .child("Loading…"),
                );
            }
            Some(list) if list.is_empty() => {
                history = history.child(
                    div()
                        .p_8()
                        .text_center()
                        .text_sm()
                        .text_color(palette.muted_foreground)
                        .child("No exports yet."),
                );
            }
            Some(list) => {
                for e in list.iter().take(5) {
                    let edition = e
                        .options
                        .get("bilingual")
                        .and_then(|v| v.as_bool())
                        .map(|b| if b { "Bilingual" } else { "Monolingual" })
                        .unwrap_or("Monolingual");
                    let mut row = div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .py_2()
                        .border_t_1()
                        .border_color(palette.border)
                        .child(
                            div()
                                .w_24()
                                .flex()
                                .flex_col()
                                .child(format!("{} · {edition}", e.format.to_uppercase())),
                        )
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .child(e.created_at.clone().unwrap_or_else(|| "—".into())),
                        )
                        .child(div().w_20().text_sm().child(format_bytes(e.size)));
                    let mut status = div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .text_sm()
                        .child(e.status.clone());
                    if let Some(err) = &e.error {
                        status = status.child(
                            div().text_sm().text_color(palette.destructive).child(err.clone()),
                        );
                    }
                    row = row.child(status);
                    if e.status == "done" {
                        let id = e.id;
                        let saving = self.saving_id == Some(id);
                        row = row.child(
                            div().w_24().child(
                                Button::new(format!("save-{id}"))
                                    .label(if saving { "Saving…" } else { "Save as" })
                                    .disabled(saving || self.saving_id.is_some())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.save_row(id, cx);
                                    })),
                            ),
                        );
                    } else {
                        row = row.child(div().w_24());
                    }
                    history = history.child(row);
                }
            }
            None => {}
        }
        col = col.child(history);
        col
    }
}
