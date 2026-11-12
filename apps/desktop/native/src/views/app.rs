//! App shell: sidebar + global/project nav + screen stack.
//!
//! Parity source: `App.tsx`, `components/layout/AppLayout.tsx`,
//! `components/layout/Navigation.tsx`, `routes/manifest.tsx`.
//!
//! Shell layout mirrors the web strata: brand header → primary action
//! (Create project) → global panels → scrollable project region → footer
//! supports a collapsed icon-only mode with tooltips; both persist to a
//! local JSON file (mirrors `wenyi.sidebarCollapsed`).

use gpui_kit::assets::IconName;
use gpui_kit::base::component_traits::Selectable as _;
use gpui_kit::component::sidebar::{
    Sidebar, SidebarFooter, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem,
    SidebarToggleButton,
};
use gpui_kit::component::theme::{ActiveTheme as _, Theme, ThemeMode as KitThemeMode};
use gpui_kit::component::{h_flex, Icon, Side};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use super::branding::brand_header;
use super::sidebar as sidebar_prefs;
use super::{
    create::Create, dashboard::Dashboard, events::EventsView, export::ExportView,
    global_settings::GlobalSettingsView, glossary::GlossaryView, progress::ProgressView,
    proofreading::ProofreadingView, style::StyleView,
};
use crate::{
    nav::{GlobalNav, ProjectNav},
    service::{Project, ServiceHandle},
    theme,
};

/// Current location: a global screen or a project screen.
#[allow(clippy::large_enum_variant)] // Project carries the open project; boxed would churn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Global(GlobalNav),
    Project {
        project: Project,
        screen: ProjectNav,
    },
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
    /// Last sidebar-prefs disk write (drags persist debounced).
    sidebar_saved_at: Option<std::time::Instant>,
    dashboard: Entity<Dashboard>,
    create: Entity<Create>,
    /// Per-project Progress views, keyed by pid (created on open).
    progress_views: std::collections::HashMap<String, Entity<ProgressView>>,
    /// Per-project Glossary views, keyed by pid (created on first visit).
    glossary_views: std::collections::HashMap<String, Entity<GlossaryView>>,
    /// Per-project Proofreading views, keyed by pid (created on first visit).
    proofreading_views: std::collections::HashMap<String, Entity<ProofreadingView>>,
    /// Per-project Style views, keyed by pid (created on first visit).
    style_views: std::collections::HashMap<String, Entity<StyleView>>,
    /// Per-project Events views, keyed by pid (created on first visit).
    event_views: std::collections::HashMap<String, Entity<EventsView>>,
    /// Per-project Export views, keyed by pid (created on first visit).
    export_views: std::collections::HashMap<String, Entity<ExportView>>,
    global_settings: Entity<GlobalSettingsView>,
    pub theme_choice: theme::ThemeChoice,
    service: Option<ServiceHandle>,
    service_error: Option<String>,
}

impl App {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Window title parity with the web app (`document.title` / index.html).
        window.set_window_title("Wenyi");
        // Open the in-process engine off the UI thread: `Service::open`
        // migrates the workspace DB, then `start()` admits jobs. Views
        // receive the shared `ServiceHandle` once ready.
        let (tx, rx) = futures::channel::oneshot::channel();
        cx.background_spawn(async move {
            let result = (|| {
                let dir = ServiceHandle::default_dir();
                let service = ServiceHandle::open(&dir).map_err(|e| e.to_string())?;
                crate::service::block_on(service.start()).map_err(|e| e.to_string())?;
                Ok::<_, String>(service)
            })();
            let _ = tx.send(result);
        })
        .detach();
        cx.spawn(async move |this, cx| {
            let result = rx.await.unwrap_or(Err("service open cancelled".into()));
            _ = this.update(cx, |app: &mut App, cx| {
                match result {
                    Ok(service) => {
                        tracing::info!("engine service ready");
                        app.service = Some(service.clone());
                        app.dashboard
                            .update(cx, |d, cx| d.set_client(service.clone(), cx));
                        app.global_settings
                            .update(cx, |g, cx| g.set_client(service.clone(), cx));
                        // AUDIT-ONLY (temporary): WENYI_AUDIT_OPEN=<pid>:<screen>
                        // opens a project screen directly for screenshots.
                        if let Ok(spec) = std::env::var("WENYI_AUDIT_OPEN") {
                            Self::audit_open(app, &spec, &service, cx);
                        }
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "engine service failed");
                        app.service_error = Some(err);
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
        // Theme selects in settings write back through this handle so mode
        // changes apply live (App::render would otherwise overwrite the
        // global choice with its stale copy on the next render).
        global_settings.update(cx, |g, _| g.set_app_handle(app_handle.clone()));
        let prefs = sidebar_prefs::load_prefs();
        Self {
            location: Location::Global(GlobalNav::Projects),
            current_project: None,
            sidebar_collapsed: prefs.collapsed,
            sidebar_width: prefs.clamped_width(),
            sidebar_saved_at: None,
            dashboard,
            create,
            global_settings,
            theme_choice: theme::load_choice(),
            glossary_views: Default::default(),
            progress_views: Default::default(),
            proofreading_views: Default::default(),
            style_views: Default::default(),
            event_views: Default::default(),
            export_views: Default::default(),
            service: None,
            service_error: None,
        }
    }

    /// AUDIT-ONLY (temporary): `WENYI_AUDIT_OPEN=<pid>:<screen>` opens a
    /// project screen directly for screenshots. Global screens via
    /// `"global:<settings|create>"`.
    fn audit_open(app: &mut App, spec: &str, service: &ServiceHandle, cx: &mut Context<Self>) {
        if let Some(global) = spec.strip_prefix("global:") {
            let nav = match global {
                "settings" => Some(GlobalNav::InterfaceSettings),
                "create" => Some(GlobalNav::CreateProject),
                _ => None,
            };
            if let Some(nav) = nav {
                app.navigate(Location::Global(nav), cx);
            }
            return;
        }
        let mut parts = spec.splitn(2, ':');
        let (Some(pid), Some(screen)) = (parts.next(), parts.next()) else {
            return;
        };
        let pid = pid.to_string();
        let screen = screen.to_string();
        let service = service.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { service.get_project(&pid) })
                .await;
            if let Ok(detail) = result {
                let nav = match screen.as_str() {
                    "proofreading" => ProjectNav::Proofreading,
                    "glossary" => ProjectNav::Glossary,
                    "style" => ProjectNav::Style,
                    "export" => ProjectNav::Export,
                    "events" => ProjectNav::Events,
                    _ => ProjectNav::Overview,
                };
                tracing::info!(screen = %screen, nav = ?nav, "audit hook navigating");
                _ = this.update(cx, |app: &mut App, cx| {
                    let project = detail.project.clone();
                    app.open_project(project.clone(), cx);
                    app.navigate(
                        Location::Project {
                            project,
                            screen: nav,
                        },
                        cx,
                    );
                });
            }
        })
        .detach();
    }

    /// Persist the sidebar prefs (best-effort; shell state only).
    fn persist_sidebar(&mut self) {
        self.sidebar_saved_at = Some(std::time::Instant::now());
        let prefs = sidebar_prefs::SidebarPrefs {
            collapsed: self.sidebar_collapsed,
            width: self.sidebar_width,
        };
        if let Err(err) = sidebar_prefs::save_prefs(&prefs) {
            tracing::warn!(error = %err, "sidebar prefs save failed");
        }
    }

    /// Persist at most every 500ms during width drags (per-pixel disk
    /// writes stutter the drag; the in-memory width stays live).
    fn persist_sidebar_debounced(&mut self) {
        let due = self
            .sidebar_saved_at
            .map(|t| t.elapsed() >= std::time::Duration::from_millis(500))
            .unwrap_or(true);
        if due {
            self.persist_sidebar();
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
        // Lazily create the per-project Progress view (needs the service).
        if !self.progress_views.contains_key(&project.id) {
            if let Some(service) = self.service.clone() {
                let pid = project.id.clone();
                let view = cx.new(|cx| {
                    let mut view = ProgressView::new(pid);
                    view.set_client(service, cx);
                    view
                });
                self.progress_views.insert(project.id.clone(), view);
            }
        }
        self.current_project = Some(project.clone());
        let location = Location::Project {
            project,
            screen: ProjectNav::Overview,
        };
        self.navigate(location, cx);
    }

    /// Navigate to `location`, pausing poll loops on the previously visible
    /// view and resuming them on the newly visible one. Only polling views
    /// (dashboard / overview / events / export / project settings) need the
    /// flag; one-shot views are unaffected.
    fn navigate(&mut self, location: Location, cx: &mut Context<Self>) {
        let old = std::mem::replace(&mut self.location, location.clone());
        self.set_location_visible(&old, false, cx);
        self.set_location_visible(&location, true, cx);
        cx.notify();
    }

    /// Apply a visibility flag to the polling view (if any) behind `location`.
    fn set_location_visible(&mut self, location: &Location, visible: bool, cx: &mut Context<Self>) {
        match location {
            Location::Global(GlobalNav::Projects) => {
                self.dashboard
                    .update(cx, |d, cx| d.set_visible(visible, cx));
            }
            Location::Project { project, screen } => match screen {
                ProjectNav::Overview => {
                    if let Some(view) = self.progress_views.get(&project.id) {
                        view.update(cx, |v, cx| v.set_visible(visible, cx));
                    }
                }
                ProjectNav::Events => {
                    if let Some(view) = self.event_views.get(&project.id) {
                        view.update(cx, |v, cx| v.set_visible(visible, cx));
                    }
                }
                ProjectNav::Export => {
                    if let Some(view) = self.export_views.get(&project.id) {
                        view.update(cx, |v, cx| v.set_visible(visible, cx));
                    }
                }
                ProjectNav::Glossary => {
                    if let Some(view) = self.glossary_views.get(&project.id) {
                        view.update(cx, |v, cx| v.set_visible(visible, cx));
                    }
                }
                _ => {}
            },
            _ => {}
        }
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
            ProjectNav::Glossary => IconName::Library,
            ProjectNav::Style => IconName::Languages,
            ProjectNav::Export => IconName::FileOutput,
            ProjectNav::Events => IconName::ScrollText,
        }
    }

    fn global_item(&self, nav: GlobalNav, cx: &mut Context<Self>) -> SidebarMenuItem {
        let active = self.location == Location::Global(nav);
        SidebarMenuItem::new(nav.label())
            .icon(Icon::new(Self::global_icon(nav)).size_4())
            .active(active)
            .collapsed(self.sidebar_collapsed)
            .on_click(cx.listener(move |this: &mut App, _, _, cx| {
                this.navigate(Location::Global(nav), cx);
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
            .icon(Icon::new(Self::project_icon(nav)).size_4())
            .active(active)
            .collapsed(self.sidebar_collapsed)
            .on_click(cx.listener(move |this: &mut App, _, _, cx| {
                this.navigate(
                    Location::Project {
                        project: project.clone(),
                        screen: nav,
                    },
                    cx,
                );
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
    /// Content row only (16px icon + label); hover/selected backgrounds come
    /// from the kit footer so only one hover rect exists.
    fn footer_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let collapsed = self.sidebar_collapsed;
        // Plain content row: the kit `SidebarFooter` owns hover + selected
        // backgrounds (a second hover rect inside produced the doubled
        // hover artifact).
        div()
            .id("sidebar-footer-settings")
            .flex()
            .w_full()
            .items_center()
            .gap_2()
            .text_sm()
            .when(collapsed, |this| this.justify_center())
            .child(Icon::new(Self::global_icon(GlobalNav::InterfaceSettings)).size_4())
            .when(!collapsed, |this| {
                this.child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .child(GlobalNav::InterfaceSettings.label()),
                )
            })
            .on_click(cx.listener(|this: &mut App, _, _, cx| {
                this.navigate(Location::Global(GlobalNav::InterfaceSettings), cx);
            }))
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // Window closed: pause running work first so jobs exit at the next
        // checkpoint with a clean `paused` status, then shut the runtime
        // down. Best-effort and bounded: never hold the close path hostage.
        // The sync engine calls run on a scoped thread; the async shutdown
        // drains in-flight checkpoint writes.
        if let Some(service) = self.service.clone() {
            std::thread::scope(|s| {
                s.spawn(|| {
                    let projects = service.list_projects().unwrap_or_default();
                    for p in projects
                        .iter()
                        .filter(|p| crate::service::is_project_busy(p.status.as_str()))
                    {
                        let _ = service.pause(&p.id);
                    }
                    crate::service::block_on(service.shutdown());
                });
            });
        }
    }
}

impl Render for App {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Keep the OS chrome in sync with the web title.
        window.set_window_title("Wenyi");
        theme::set_choice(self.theme_choice);
        let system_dark = theme::system_is_dark(cx);
        theme::set_system_dark(system_dark);
        // Keep the kit chrome (sidebar/buttons/inputs/dialogs) in the same
        // mode as our palette; guarded so steady state costs one comparison.
        let dark = self.theme_choice.mode.resolve_dark(system_dark);
        let kit_dark = cx.theme().mode.is_dark();
        if dark != kit_dark {
            Theme::change(
                if dark {
                    KitThemeMode::Dark
                } else {
                    KitThemeMode::Light
                },
                None,
                cx,
            );
        }
        let palette = theme::current_palette();
        let collapsed = self.sidebar_collapsed;
        let expanded_width = px(sidebar_prefs::clamp_width(self.sidebar_width));
        // Brand header row: wordmark + collapse toggle. Collapsed, the brand
        // hides (web behaviour) and the toggle centers in the 64px rail.
        let header = SidebarHeader::new().child(
            h_flex()
                .w_full()
                .items_center()
                .when(collapsed, |this| this.justify_center())
                .when(!collapsed, |this| {
                    this.justify_between().child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(brand_header(collapsed, palette)),
                    )
                })
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

        // Global panels group (Projects). Collapse state is owned by the
        // `Sidebar` below — kit propagates it to groups/menus/items (labels
        // hide, icons center, tooltips arm).
        let global_menu = SidebarMenu::new().children(self.global_panel_items(cx));
        let global_group = SidebarGroup::new("").child(global_menu);

        // Project nav group (when a project is open) — audience-filtered,
        // with the project name as the section header (mirrors the web
        // `ProjectNavigation` context label). Preserved across navigation:
        // it reads `current_project`, not the in-progress location.
        let project_groups: Vec<SidebarGroup<SidebarMenu>> =
            if let Some(project) = self.current_project.clone() {
                let items: Vec<SidebarMenuItem> = ProjectNav::visible_for()
                    .into_iter()
                    .map(|nav| self.project_item(&project, nav, cx))
                    .collect();
                vec![SidebarGroup::new(project.name.clone())
                    .children([SidebarMenu::new().children(items)])]
            } else {
                Vec::new()
            };

        // Primary action row under the header (web `action` slot).
        let action_group =
            SidebarGroup::new("").child(SidebarMenu::new().children([self.action_item(cx)]));

        // Footer: Interface settings pinned to the bottom (web `foot` slot).
        // Selected state goes to the kit footer (single hover/selected rect).
        let settings_active = self.location == Location::Global(GlobalNav::InterfaceSettings);
        let footer = SidebarFooter::new()
            .selected(settings_active)
            .child(self.footer_row(cx));

        // Kit-owned collapse: animated clip-width wrapper, group labels hide,
        // items center. The expanded width stays draggable (persisted).
        let sidebar = Sidebar::new("nav")
            .side(Side::Left)
            .collapsible(true)
            .collapsed(collapsed)
            .header(header)
            .footer(footer)
            .w(expanded_width)
            .child(action_group)
            .child(global_group)
            .children(project_groups);

        // Drag handle: resizing the sidebar edge updates + persists width.
        // Hidden while collapsed (the toggle restores the stored width).
        let shell = h_flex().size_full().child(
            div()
                .h_full()
                .flex_shrink_0()
                .min_h_0()
                // Kit collapses to a 48px icon rail; the wrapper must not
                // clamp wider or the collapse animation fights the layout.
                .min_w(px(48.0))
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
                        this.persist_sidebar_debounced();
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
                    .child(match (&self.service_error, &self.service, &self.location) {
                        (Some(err), _, _) => div()
                            .flex_1()
                            .items_center()
                            .justify_center()
                            .child(format!("Engine failed: {err}"))
                            .into_any_element(),
                        (None, None, _) => div()
                            .flex_1()
                            .items_center()
                            .justify_center()
                            .child("Starting local engine…")
                            .into_any_element(),
                        (None, Some(_), Location::Global(GlobalNav::Projects)) => div()
                            .flex_1()
                            .min_h_0()
                            .child(self.dashboard.clone())
                            .into_any_element(),
                        (None, Some(_), Location::Global(GlobalNav::CreateProject)) => div()
                            .flex_1()
                            .min_h_0()
                            .child(self.create.clone())
                            .into_any_element(),
                        (None, Some(_), Location::Global(GlobalNav::InterfaceSettings)) => div()
                            .flex_1()
                            .min_h_0()
                            .child(self.global_settings.clone())
                            .into_any_element(),
                        (None, Some(service), Location::Project { project, screen }) => {
                            match screen {
                                ProjectNav::Overview => {
                                    match self.progress_views.get(&project.id) {
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
                                    }
                                }
                                ProjectNav::Glossary => {
                                    if !self.glossary_views.contains_key(&project.id) {
                                        let pid = project.id.clone();
                                        let view = cx.new(|cx| {
                                            let mut view = GlossaryView::new(pid, window, cx);
                                            view.set_client(service.clone(), cx);
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
                                ProjectNav::Proofreading => {
                                    if !self.proofreading_views.contains_key(&project.id) {
                                        let pid = project.id.clone();
                                        let view = cx.new(|cx| {
                                            let mut view = ProofreadingView::new(pid, window, cx);
                                            view.set_client(service.clone(), cx);
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
                                ProjectNav::Style => {
                                    if !self.style_views.contains_key(&project.id) {
                                        let pid = project.id.clone();
                                        let view = cx.new(|cx| {
                                            let mut view = StyleView::new(pid);
                                            view.set_client(service.clone(), cx);
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
                                ProjectNav::Events => {
                                    if !self.event_views.contains_key(&project.id) {
                                        let pid = project.id.clone();
                                        let view = cx.new(|cx| {
                                            let mut view = EventsView::new(pid, window, cx);
                                            view.set_client(service.clone(), cx);
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
                                    if !self.export_views.contains_key(&project.id) {
                                        let pid = project.id.clone();
                                        let view = cx.new(|cx| {
                                            let mut view = ExportView::new(pid, window, cx);
                                            view.set_client(service.clone(), cx);
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
                            }
                        }
                    }),
            )
    }
}
