//! Brand header for the app shell — port of `Brand()` in
//! `components/layout/AppLayout.tsx`.
//!
//! Web source renders the `wenyi-emblem.png` asset next to the "Wenyi"
//! wordmark. The native shell has no bundled emblem asset, so this draws a
//! styled wordmark: an accent badge with the "文" glyph plus "Wenyi" text.
//! When collapsed, only the badge shows.
use gpui_kit::component::h_flex;
use gpui_kit::*;

use crate::theme;

/// Brand header: badge + wordmark (badge only when collapsed).
pub fn brand_header(collapsed: bool, palette: theme::Palette) -> impl IntoElement {
    let badge = div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .w(px(36.0))
        .h(px(36.0))
        .rounded(px(9.0))
        .bg(palette.primary)
        .text_color(palette.primary_foreground)
        .text_lg()
        .child("文");
    if collapsed {
        div()
            .flex()
            .items_center()
            .justify_center()
            .w_full()
            .child(badge)
            .into_any_element()
    } else {
        h_flex()
            .items_center()
            .gap_2()
            .child(badge)
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::NORMAL)
                    .text_color(palette.foreground)
                    .child("Wenyi"),
            )
            .into_any_element()
    }
}
