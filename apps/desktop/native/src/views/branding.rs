//! Brand header for the app shell — port of `Brand()` in
//! `components/layout/AppLayout.tsx`.
//!
//! Web source renders the `wenyi-emblem.png` asset next to the "Wenyi"
//! wordmark. The native shell has no image-asset pipeline (GPUI `img`
//! needs bundled resources), so this renders the wordmark alone in the
//! same 20px size. When collapsed the brand hides entirely (web collapses
//! it to zero width, leaving only the toggle button).
use gpui_kit::*;

use crate::theme;

/// Brand header: "Wenyi" wordmark, or nothing when collapsed.
pub fn brand_header(collapsed: bool, palette: theme::Palette) -> impl IntoElement {
    if collapsed {
        div().into_any_element()
    } else {
        div()
            .text_size(px(20.0))
            .font_weight(FontWeight::NORMAL)
            .text_color(palette.foreground)
            .child("Wenyi")
            .into_any_element()
    }
}
