//! App shell: sidebar + global/project nav + screen stack.
//!
//! Parity source: `App.tsx`, `components/layout/AppLayout.tsx`,
//! `components/layout/Navigation.tsx`, `routes/manifest.tsx`.
//!
//! Shell layout mirrors the web strata: brand header → primary action
//! (Create project) → global panels → scrollable project region → footer
//! supports a collapsed icon-only mode with tooltips; both persist to a
//! local JSON file (mirrors `wenyi.sidebarCollapsed`).

use gpui_kit::component::sidebar::{
    Sidebar, SidebarFooter, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem,
    SidebarToggleButton,
};
use gpui_kit::component::{Icon, Side, Sizable as _, h_flex};
use gpui_kit::assets::IconName;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use super::branding::brand_header;
use super::sidebar as sidebar_prefs;
use super::{
    contents::ContentsView, create::Create, dashboard::Dashboard, events::EventsView,
    export::ExportView, global_settings::GlobalSettingsView, glossary::GlossaryView,
    progress::ProgressView, proj_settings::ProjSettingsView, proofreading::ProofreadingView,
    review::ReviewView, style::StyleView, subtitles::SubtitlesView,
};
use crate::{
    backend::Backend,
    client::{Client, Project},
    nav::{GlobalNav, ProjectNav},
    theme,
};

/// Current location: a global screen or a project screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Global(GlobalNav),
    Project { project: Project, screen: ProjectNav },
}

/// Drag payload for the sidebar-width handle.
#[derive(Clone)]
struct SidebarDrag;

/// Invisible drag view required by the drag constructor.
struct ResizeDragView;

impl Render for ResizeDragView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}
pub struct App {
    location: Location,
    /// Last opened project (mirrors `wenyi.lastProject` recall).
    current_project: Option<Project>,
    /// Collapsed icon-only mode (mirrors `wenyi.sidebarCollapsed`).
    sidebar_collapsed: bool,
    /// Expanded sidebar width in px (drag-persisted).
    sidebar_width: f32,
    dashboard: Entity<Dashboard>,
    create: Entity<Create>,
    /// Per-project Progress views, keyed by pid (created on open).
    progress_views: std::collections::HashMap<String, Entity<ProgressView>>,
    /// Per-project Glossary views, keyed by pid (created on first visit).
    glossary_views: std::collections::HashMap<String, Entity<GlossaryView>>,
    /// Per-project Contents views, keyed by pid (created on first visit).
    contents_views: std::collections::HashMap<String, Entity<ContentsView>>,
    /// Per-project Proofreading views, keyed by pid (created on first visit).
    proofreading_views: std::collections::HashMap<String, Entity<ProofreadingView>>,
    /// Per-project Style views, keyed by pid (created on first visit).
    style_views: std::collections::HashMap<String, Entity<StyleView>>,
    /// Per-project Review views, keyed by pid (created on first visit).
    review_views: std::collections::HashMap<String, Entity<ReviewView>>,
    /// Per-project Subtitles views, keyed by pid (created on first visit).
    subtitle_views: std::collections::HashMap<String, Entity<SubtitlesView>>,
    /// Per-project Events views, keyed by pid (created on first visit).
    event_views: std::collections::HashMap<String, Entity<EventsView>>,
    /// Per-project Export views, keyed by pid (created on first visit).
    export_views: std::collections::HashMap<String, Entity<ExportView>>,
    /// Per-project Settings views, keyed by pid (created on first visit).
    settings_views: std::collections::HashMap<String, Entity<ProjSettingsView>>,
    global_settings: Entity<GlobalSettingsView>,
    pub theme_choice: theme::ThemeChoice,
    _backend: Option<Backend>,
    client: Option<Client>,
    backend_error: Option<String>,
}

impl App {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Window title parity with the web app (`document.title` / index.html).
        window.set_window_title("Wenyi");
        // Attach mode (--backend/--token): connect directly, no spawn.
        // Otherwise spawn the sidecar off the UI thread, then build the client.
        // Dev: set WENYI_DESKTOP_PYTHON to a venv python for `python -m wenyi_desktop`.
        let attach = std::env::var("WENYI_NATIVE_BACKEND")
            .ok()
            .zip(std::env::var("WENYI_NATIVE_TOKEN").ok());
        let data_dir = std::env::var("WENYI_NATIVE_DATA_DIR").ok();
        let (tx, rx) = futures::channel::oneshot::channel();
        cx.background_spawn(async move {
            let result: Result<(Option<Backend>, Client), String> = match attach {
                Some((api_base, token)) => Ok((None, Client::new(api_base, token))),
                None => Backend::spawn(data_dir.as_deref().map(std::path::Path::new))
                    .map(|backend| {
                        let client = Client::new(
                            backend.connection.api_base.clone(),
                            backend.connection.token.clone(),
                        );
                        (Some(backend), client)
                    })
                    .map_err(|e| e.to_string()),
            };
            let _ = tx.send(result);
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or(Err("sidecar spawn cancelled".into()));
            _ = this.update(cx, |app: &mut App, cx| {
                match result {
                    Ok((backend, client)) => {
                        tracing::info!("backend connected");
                        app._backend = backend;
                        app.client = Some(client.clone());
                        app.dashboard.update(cx, |d, cx| d.set_client(client.clone(), cx));
                        app.create.update(cx, |c, cx| c.set_client(client.clone(), cx));
                        app.global_settings.update(cx, |g, cx| g.set_client(client.clone(), cx));
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "backend failed");
                        app.backend_error = Some(err);
                    }
                }
                cx.notify();
            });
        })
        .detach();

        let app_handle = cx.entity().downgrade();
        let dashboard = cx.new(|_| {
            Dashboard::new().on_open(move |project, _window, cx| {
                _ = app_handle.update(cx, |app: &mut App, cx| {
                    app.open_project(project, cx);
                });
            })
        });
        let create = cx.new(|cx| Create::new(window, cx));
        let global_settings = cx.new(|cx| GlobalSettingsView::new(window, cx));
        let app_handle = cx.entity().downgrade();
        let prefs = sidebar_prefs::load_prefs();
        Self {
            location: Location::Global(GlobalNav::Projects),
            current_project: None,
            sidebar_collapsed: prefs.collapsed,
            sidebar_width: prefs.clamped_width(),
            dashboard,
            create,
            global_settings,
            theme_choice: theme::load_choice(),
            glossary_views: Default::default(),
            progress_views: Default::default(),
            contents_views: Default::default(),
            proofreading_views: Default::default(),
            style_views: Default::default(),
            review_views: Default::default(),
            subtitle_views: Default::default(),
            event_views: Default::default(),
            export_views: Default::default(),
            settings_views: Default::default(),
            _backend: None,
            client: None,
            backend_error: None,
        }
    }

    /// Persist the sidebar prefs (best-effort; shell state only).
    fn persist_sidebar(&self) {
        let prefs = sidebar_prefs::SidebarPrefs {
            collapsed: self.sidebar_collapsed,
            width: self.sidebar_width,
        };
        if let Err(err) = sidebar_prefs::save_prefs(&prefs) {
            tracing::warn!(error = %err, "sidebar prefs save failed");
        }
    }

    /// Toggle collapsed icon-only mode (sidebar header button).
    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        self.persist_sidebar();
        cx.notify();
    }

    /// Open a project (Dashboard card click) — mirrors router navigate.
    pub fn open_project(&mut self, project: Project, cx: &mut Context<Self>) {
        // Lazily create the per-project Progress view (needs client).
        if !self.progress_views.contains_key(&project.id) {
            if let Some(client) = self.client.clone() {
                let pid = project.id.clone();
                let view = cx.new(|cx| {
                    let mut view = ProgressView::new(pid);
                    view.set_client(client, cx);
                    view
                });
                self.progress_views.insert(project.id.clone(), view);
            }
        }
        self.current_project = Some(project.clone());
        self.location = Location::Project {
            project,
            screen: ProjectNav::Overview,
        };
        cx.notify();
    }

    /// Map a [`GlobalNav`] entry to its Lucide icon.
    fn global_icon(nav: GlobalNav) -> IconName {
        match nav {
            GlobalNav::Projects => IconName::LayoutDashboard,
            GlobalNav::CreateProject => IconName::FolderPlus,
            GlobalNav::InterfaceSettings => IconName::Settings,
        }
    }

    /// Map a [`ProjectNav`] entry to its Lucide icon.
    fn project_icon(nav: ProjectNav) -> IconName {
        match nav {
            ProjectNav::Overview => IconName::Sparkles,
            ProjectNav::Proofreading => IconName::BookOpenCheck,
            ProjectNav::Review => IconName::ListChecks,
            ProjectNav::Subtitles => IconName::Captions,
            ProjectNav::Glossary => IconName::Library,
            ProjectNav::Style => IconName::Languages,
            ProjectNav::Contents => IconName::ListTree,
            ProjectNav::Export => IconName::FileOutput,
            ProjectNav::Settings => IconName::Settings2,
            ProjectNav::Events => IconName::ScrollText,
        }
    }

    fn global_item(&self, nav: GlobalNav, cx: &mut Context<Self>) -> SidebarMenuItem {
        let active = self.location == Location::Global(nav);
        SidebarMenuItem::new(nav.label())
            .icon(Icon::new(Self::global_icon(nav)))
            .active(active)
            .collapsed(self.sidebar_collapsed)
            .on_click(cx.listener(move |this: &mut App, _, _, cx| {
                this.location = Location::Global(nav);
                cx.notify();
            }))
    }

    fn project_item(
        &self,
        project: &Project,
        nav: ProjectNav,
        cx: &mut Context<Self>,
    ) -> SidebarMenuItem {
        let active = self.location
            == Location::Project {
                project: project.clone(),
                screen: nav,
            };
        let project = project.clone();
        SidebarMenuItem::new(nav.label())
            .icon(Icon::new(Self::project_icon(nav)))
            .active(active)
            .collapsed(self.sidebar_collapsed)
            .on_click(cx.listener(move |this: &mut App, _, _, cx| {
                this.location = Location::Project {
                    project: project.clone(),
                    screen: nav,
                };
                cx.notify();
            }))
    }

    /// Global panel entries (Projects) — the foot slot renders separately,
    /// pinned to the bottom.
    fn global_panel_items(&self, cx: &mut Context<Self>) -> Vec<SidebarMenuItem> {
        GlobalNav::panel()
            .into_iter()
            .map(|nav| self.global_item(nav, cx))
            .collect()
    }

    /// Primary action entry (Create project) — the web `action` slot;
    /// rendered as the sidebar header button row on desktop.
    fn action_item(&self, cx: &mut Context<Self>) -> SidebarMenuItem {
        self.global_item(GlobalNav::CreateProject, cx)
    }

    /// Footer entry (Interface settings) — pinned via `Sidebar::footer`.
    /// A plain row (the kit footer slot takes `IntoElement`, not menus).
    fn footer_row(&self, palette: theme::Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.location == Location::Global(GlobalNav::InterfaceSettings);
        div()
            .id("sidebar-footer-settings")
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .h_8()
            .rounded_md()
            .when(active, |this| this.bg(palette.accent))
            .when(!self.sidebar_collapsed, |this| {
                this.child(GlobalNav::InterfaceSettings.label())
            })
            .on_click(cx.listener(|this: &mut App, _, _, cx| {
                this.location = Location::Global(GlobalNav::InterfaceSettings);
                cx.notify();
            }))
    }
}

impl Render for App {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Keep the OS chrome in sync with the web title.
        window.set_window_title("Wenyi");
        theme::set_choice(self.theme_choice);
        theme::set_system_dark(theme::system_is_dark(cx));
        let palette = theme::current_palette();
        let collapsed = self.sidebar_collapsed;
        let expanded_width = px(sidebar_prefs::clamp_width(self.sidebar_width));
        // Brand header row: wordmark + collapse toggle.
        let header = SidebarHeader::new().child(
            h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .child(div().flex_1().min_w_0().child(brand_header(collapsed, palette)))
                .child(
                    SidebarToggleButton::new()
                        .side(Side::Left)
                        .collapsed(collapsed)
                        .accessibility_label(if collapsed {
                            "Expand sidebar"
                        } else {
                            "Collapse sidebar"
                        })
                        .on_click(cx.listener(|this: &mut App, _, _, cx| {
                            this.toggle_sidebar(cx);
                        })),
                ),
        );

        // Global panels group (Projects).
        let global_menu = SidebarMenu::new().children(self.global_panel_items(cx));
        let global_group = SidebarGroup::new("").child(global_menu);

        // Project nav group (when a project is open) — audience-filtered,
        // with the project name as the section header (mirrors the web
        // `ProjectNavigation` context label). Preserved across navigation:
        // it reads `current_project`, not the in-progress location.
        let project_groups: Vec<SidebarGroup<SidebarMenu>> =
            if let Some(project) = self.current_project.clone() {
                let items: Vec<SidebarMenuItem> = ProjectNav::visible_for(&project)
                    .into_iter()
                    .map(|nav| self.project_item(&project, nav, cx))
                    .collect();
                vec![
                    SidebarGroup::new(project.name.clone()).children([SidebarMenu::new()
                        .children(items)]),
                ]
            } else {
                Vec::new()
            };

        // Primary action row under the header (web `action` slot).
        let action_group = SidebarGroup::new("").child(
            SidebarMenu::new().children([self.action_item(cx)]),
        );

        // Footer: Interface settings pinned to the bottom (web `foot` slot).
        let footer = SidebarFooter::new().child(self.footer_row(palette, cx));

        let sidebar_width = if collapsed {
            px(sidebar_prefs::COLLAPSED_WIDTH)
        } else {
            expanded_width
        };
        let sidebar = Sidebar::new("nav")
            .side(Side::Left)
            .collapsible(false)
            .header(header)
            .footer(footer)
            .w(sidebar_width)
            .child(action_group)
            .child(global_group)
            .children(project_groups);

        // Drag handle: resizing the sidebar edge updates + persists width.
        // Hidden while collapsed (the toggle restores the stored width).
        let shell = h_flex()
            .size_full()
            .child(
                div()
                    .h_full()
                    .flex_shrink_0()
                    .min_h_0()
                    .min_w(px(sidebar_prefs::COLLAPSED_WIDTH))
                    .child(sidebar),
            );
        let shell = if collapsed {
            shell
        } else {
            let app = cx.entity().downgrade();
            shell.child(
                gpui_kit::component::resize_handle::<SidebarDrag, ResizeDragView>(
                    "sidebar-resize-handle",
                    Axis::Horizontal,
                )
                .on_drag(SidebarDrag, |_, _, _, cx| cx.new(|_| ResizeDragView))
                .on_drag_move::<SidebarDrag>(move |event, _, cx| {
                    // Handle sits at the sidebar edge: cursor x == new width.
                    let width: f32 = event.event.position.x.into();
                    _ = app.update(cx, |this: &mut App, cx| {
                        this.sidebar_width = sidebar_prefs::clamp_width(width);
                        this.persist_sidebar();
                        cx.notify();
                    });
                }),
            )
        };

        shell
            .bg(palette.background)
            .text_color(palette.foreground)
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .overflow_hidden()
                    .child(match (&self.backend_error, &self.client, &self.location) {
                        (Some(err), _, _) => div()
                            .flex_1()
                            .items_center()
                            .justify_center()
                            .child(format!("Backend failed: {err}"))
                            .into_any_element(),
                        (None, None, _) => div()
                            .flex_1()
                            .items_center()
                            .justify_center()
                            .child("Starting local engine…")
                            .into_any_element(),
                        (None, Some(_), Location::Global(GlobalNav::Projects)) => {
                            div().flex_1().min_h_0().child(self.dashboard.clone()).into_any_element()
                        }
                        (None, Some(_), Location::Global(GlobalNav::CreateProject)) => {
                            div().flex_1().min_h_0().child(self.create.clone()).into_any_element()
                        }
                        (None, Some(_), Location::Global(GlobalNav::InterfaceSettings)) => {
                            div().flex_1().min_h_0().child(self.global_settings.clone()).into_any_element()
                        }
                        (None, Some(client), Location::Project { project, screen }) => {
                            match screen {
                                ProjectNav::Overview => match self.progress_views.get(&project.id)
                                {
                                    Some(view) => div()
                                        .flex_1()
                                        .min_h_0()
                                        .child(view.clone())
                                        .into_any_element(),
                                    None => div()
                                        .flex_1()
                                        .items_center()
                                        .justify_center()
                                        .child("Loading overview…")
                                        .into_any_element(),
                                },
                                ProjectNav::Glossary => {
                                    // Lazily create on first visit (needs Window).
                                    if !self.glossary_views.contains_key(&project.id) {
                                        let pid = project.id.clone();
                                        let view = cx.new(|cx| {
                                            let mut view = GlossaryView::new(pid, window, cx);
                                            view.set_client(client.clone(), cx);
                                            view
                                        });
                                        self.glossary_views.insert(project.id.clone(), view);
                                    }
                                    div()
                                        .flex_1()
                                        .min_h_0()
                                        .child(self.glossary_views[&project.id].clone())
                                        .into_any_element()
                                }
                                ProjectNav::Contents => {
                                    // Lazily create on first visit (needs Window).
                                    if !self.contents_views.contains_key(&project.id) {
                                        let pid = project.id.clone();
                                        let view = cx.new(|cx| {
                                            let mut view = ContentsView::new(pid, window, cx);
                                            view.set_client(client.clone(), cx);
                                            view
                                        });
                                        self.contents_views.insert(project.id.clone(), view);
                                    }
                                    div()
                                        .flex_1()
                                        .min_h_0()
                                        .child(self.contents_views[&project.id].clone())
                                        .into_any_element()
                                }
                                ProjectNav::Proofreading => {
                                    // Lazily create on first visit (needs Window).
                                    if !self.proofreading_views.contains_key(&project.id) {
                                        let pid = project.id.clone();
                                        let view = cx.new(|cx| {
                                            let mut view = ProofreadingView::new(pid, window, cx);
                                            view.set_client(client.clone(), cx);
                                            view
                                        });
                                        self.proofreading_views.insert(project.id.clone(), view);
                                    }
                                    div()
                                        .flex_1()
                                        .min_h_0()
                                        .child(self.proofreading_views[&project.id].clone())
                                        .into_any_element()
                                }
                                ProjectNav::Review => {
                                    // Lazily create on first visit (needs Window).
                                    if !self.review_views.contains_key(&project.id) {
                                        let pid = project.id.clone();
                                        let view = cx.new(|cx| {
                                            let mut view = ReviewView::new(pid, window, cx);
                                            view.set_client(client.clone(), cx);
                                            view
                                        });
                                        self.review_views.insert(project.id.clone(), view);
                                    }
                                    div()
                                        .flex_1()
                                        .min_h_0()
                                        .child(self.review_views[&project.id].clone())
                                        .into_any_element()
                                }
                                ProjectNav::Style => {
                                    // Lazily create on first visit (needs Window).
                                    if !self.style_views.contains_key(&project.id) {
                                        let pid = project.id.clone();
                                        let view = cx.new(|cx| {
                                            let mut view = StyleView::new(pid);
                                            view.set_client(client.clone(), cx);
                                            view
                                        });
                                        self.style_views.insert(project.id.clone(), view);
                                    }
                                    div()
                                        .flex_1()
                                        .min_h_0()
                                        .child(self.style_views[&project.id].clone())
                                        .into_any_element()
                                }
                                ProjectNav::Subtitles => {
                                    // Lazily create on first visit (needs Window).
                                    if !self.subtitle_views.contains_key(&project.id) {
                                        let pid = project.id.clone();
                                        let view = cx.new(|cx| {
                                            let mut view = SubtitlesView::new(pid, window, cx);
                                            view.set_client(client.clone(), cx);
                                            view
                                        });
                                        self.subtitle_views.insert(project.id.clone(), view);
                                    }
                                    div()
                                        .flex_1()
                                        .min_h_0()
                                        .child(self.subtitle_views[&project.id].clone())
                                        .into_any_element()
                                }
                                ProjectNav::Events => {
                                    // Lazily create on first visit (needs Window).
                                    if !self.event_views.contains_key(&project.id) {
                                        let pid = project.id.clone();
                                        let view = cx.new(|cx| {
                                            let mut view = EventsView::new(pid, window, cx);
                                            view.set_client(client.clone(), cx);
                                            view
                                        });
                                        self.event_views.insert(project.id.clone(), view);
                                    }
                                    div()
                                        .flex_1()
                                        .min_h_0()
                                        .child(self.event_views[&project.id].clone())
                                        .into_any_element()
                                }
                                ProjectNav::Export => {
                                    // Lazily create on first visit (needs Window).
                                    if !self.export_views.contains_key(&project.id) {
                                        let pid = project.id.clone();
                                        let view = cx.new(|cx| {
                                            let mut view = ExportView::new(pid, window, cx);
                                            view.set_client(client.clone(), cx);
                                            view
                                        });
                                        self.export_views.insert(project.id.clone(), view);
                                    }
                                    div()
                                        .flex_1()
                                        .min_h_0()
                                        .child(self.export_views[&project.id].clone())
                                        .into_any_element()
                                }
                                ProjectNav::Settings => {
                                    // Lazily create on first visit (needs Window).
                                    if !self.settings_views.contains_key(&project.id) {
                                        let pid = project.id.clone();
                                        let view = cx.new(|cx| {
                                            let mut view = ProjSettingsView::new(pid, window, cx);
                                            view.set_client(client.clone(), cx);
                                            view
                                        });
                                        self.settings_views.insert(project.id.clone(), view);
                                    }
                                    div()
                                        .flex_1()
                                        .min_h_0()
                                        .child(self.settings_views[&project.id].clone())
                                        .into_any_element()
                                }
                                _ => div()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .p_6()
                                    .gap_2()
                                    .child(div().text_xl().font_weight(FontWeight::BOLD).child(
                                        format!("{} — {}", project.name, screen.label()),
                                    ))
                                    .child(format!(
                                        "Screen “{}” for project {} — coming in its milestone.",
                                        screen.label(),
                                        project.id
                                    ))
                                    .into_any_element(),
                            }
                        }
                    }),
            )
    }
}
