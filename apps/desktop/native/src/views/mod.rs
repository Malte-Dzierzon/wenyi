//! View layer: app shell + screens.
//!
//! U1 milestone: shell (sidebar + global nav) + Dashboard (screen 1).
//! Parity source: `packages/ui/src/{App.tsx,routes/manifest.tsx,
//! features/dashboard/Dashboard.tsx}`.
mod app;
mod branding;
mod contents;
mod create;
mod dashboard;
mod dialog;
mod events;
mod export;
mod glossary;
mod global_settings;
mod progress;
mod proj_settings;
mod proofreading;
mod review;
mod sidebar;
mod style;
mod subtitles;
mod toast;
mod widgets;

pub use app::App;
pub(crate) use dialog::confirm_destructive;
pub(crate) use toast::{notify_error, notify_info, notify_success, notify_warning};
