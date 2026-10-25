//! Confirm-dialog helper: modal destructive confirm over the kit dialog layer.
//!
//! Wraps `window.open_alert_dialog` (kit default `keyboard: true`, so Esc
//! closes with no effect; show-cancel confirm variant). Reduced motion is
//! honored inside the kit (`cx.reduce_motion()`); views keep no custom
//! transitions. The `on_confirm` callback runs on OK and returns whether the
//! dialog should close (`true` closes).

use std::rc::Rc;

use gpui_kit::component::button::ButtonVariant;
use gpui_kit::component::WindowExt;
use gpui_kit::{App, SharedString, Window};

/// Open a destructive confirm dialog (e.g. delete). Cancel/Esc closes with
/// no effect; OK runs `on_confirm`.
pub fn confirm_destructive(
    window: &mut Window,
    cx: &mut App,
    title: impl Into<SharedString>,
    message: impl Into<SharedString>,
    ok_label: impl Into<SharedString>,
    on_confirm: impl Fn(&mut Window, &mut App) -> bool + 'static,
) {
    let title: SharedString = title.into();
    let message: SharedString = message.into();
    let ok_label: SharedString = ok_label.into();
    let on_confirm: Rc<dyn Fn(&mut Window, &mut App) -> bool> = Rc::new(on_confirm);
    window.open_alert_dialog(cx, move |alert, _, _| {
        let on_confirm = on_confirm.clone();
        alert
            .confirm()
            .title(title.clone())
            .description(message.clone())
            .ok_text(ok_label.clone())
            .ok_variant(ButtonVariant::Danger)
            .on_ok(move |_, window, cx| on_confirm(window, cx))
    });
}
