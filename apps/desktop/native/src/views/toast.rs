//! Toast helpers over the kit notification layer.
//!
//! Thin wrappers around `window.push_notification`; the `NotificationList`
//! host is already mounted by the kit window state (`open_window` + `init`
//! in `main.rs`), so views only call these. Autohide/delivery fall back to
//! the kit theme defaults.

use gpui_kit::component::WindowExt;
use gpui_kit::component::notification::Notification;
use gpui_kit::{App, SharedString, Window};

/// Toast: operation succeeded.
pub fn notify_success(window: &mut Window, cx: &mut App, message: impl Into<SharedString>) {
    window.push_notification(Notification::success(message), cx);
}

/// Toast: operation failed. Inline `ErrorNotice` panels stay as the primary
/// error surface; toasts are the transient companion.
pub fn notify_error(window: &mut Window, cx: &mut App, message: impl Into<SharedString>) {
    window.push_notification(Notification::error(message), cx);
}

/// Toast: informational note.
pub fn notify_info(window: &mut Window, cx: &mut App, message: impl Into<SharedString>) {
    window.push_notification(Notification::info(message), cx);
}

/// Toast: warning that needs transient attention.
pub fn notify_warning(window: &mut Window, cx: &mut App, message: impl Into<SharedString>) {
    window.push_notification(Notification::warning(message), cx);
}
