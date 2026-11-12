//! View layer: app shell + screens.

mod app;
mod branding;
mod create;
mod dashboard;
mod dialog;
mod events;
mod export;
mod global_settings;
mod glossary;
mod progress;
mod proofreading;
mod sidebar;
mod style;
mod toast;
pub(crate) mod ui;
// Staged for the progress view (usage/history render behind the engine
// service); the data layer is covered by unit tests. Allowed until wired.
#[allow(dead_code)]
mod widgets;

pub use app::App;
pub(crate) use dialog::confirm_destructive;
pub(crate) use toast::{notify_error, notify_success};
