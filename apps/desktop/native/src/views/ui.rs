//! Shared editorial UI primitives for native screens.
//!
//! One implementation per element: page titles, section headings, status
//! badges, list containers, error boxes, empty states. Views compose these
//! instead of restating sizes, weights, and paddings inline.
//!
//! All helpers are pure element builders (no entities, no fetches) so they
//! cost nothing outside render and stay safe to call per frame.
use gpui_kit::component::StyledExt;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::theme;

/// Page header: 20px semibold title + optional muted caption (context, counts,
/// project breadcrumb). Replaces the 14× ad-hoc
/// `.text_xl().font_weight(BOLD)` titles.
pub fn page_title(title: impl IntoElement, caption: Option<impl IntoElement>) -> impl IntoElement {
    let t = theme::text::title();
    let mut col = div()
        .flex()
        .flex_col()
        .gap_0()
        .child(
            div()
                .text_size(t.size)
                .font_weight(t.weight)
                .font(theme::fonts::ui())
                .child(title),
        );
    if let Some(caption) = caption {
        let c = theme::text::caption();
        col = col.child(
            div()
                .text_size(c.size)
                .text_color(theme::current_palette().muted_foreground)
                .child(caption),
        );
    }
    col
}

/// Section heading inside a page: 16px semibold + optional description.
pub fn section_title(title: impl IntoElement, desc: Option<impl IntoElement>) -> impl IntoElement {
    let h = theme::text::h2();
    let mut col = div().flex().flex_col().gap_0().child(
        div()
            .text_size(h.size)
            .font_weight(h.weight)
            .font(theme::fonts::ui())
            .child(title),
    );
    if let Some(desc) = desc {
        let c = theme::text::caption();
        col = col.child(
            div()
                .text_size(c.size)
                .text_color(theme::current_palette().muted_foreground)
                .child(desc),
        );
    }
    col
}

/// Status → badge (fill, text). Mirrors `badge.tsx` variants: neutral pair
/// for unknown states, translucent tint fill with the tone as text.
pub fn status_badge(status: &str, palette: &theme::Palette) -> (Hsla, Hsla) {
    let tone = match status {
        "prepared" | "done" | "completed" | "reviewed" | "applied" | "published" => {
            Some(palette.success)
        }
        "pausing" | "paused" | "interrupted" => Some(palette.warning),
        "failed" | "error" => Some(palette.destructive),
        "running" | "parsing" | "preparing" | "translating" | "reviewing" | "autofixing"
        | "postprocessing" => Some(palette.info),
        _ => None,
    };
    match tone {
        Some(t) => (Hsla { a: 0.12, ..t }, t),
        None => (palette.secondary, palette.secondary_foreground),
    }
}

/// Rendered status pill (shared with [`status_badge`] tones).
pub fn status_pill(status: &str, palette: &theme::Palette) -> impl IntoElement {
    let (bg, fg) = status_badge(status, palette);
    div()
        .px_2()
        .py_px()
        .rounded_sm()
        .bg(bg)
        .text_color(fg)
        .text_xs()
        .child(status.to_string())
}

/// List container: single bordered card with hidden overflow; children add
/// their own `border_t_1 separator` dividers. One surface per list instead of
/// a card per row.
pub fn list_container(palette: &theme::Palette) -> Div {
    div()
        .flex()
        .flex_col()
        .rounded_sm()
        .border_1()
        .border_color(palette.border)
        .bg(palette.card)
        .overflow_hidden()
}

/// Table header row inside a [`list_container`]: muted 12px medium labels.
pub fn list_header(palette: &theme::Palette) -> Div {
    div()
        .flex()
        .gap_3()
        .px_3()
        .py_2()
        .text_xs()
        .font_medium()
        .text_color(palette.muted_foreground)
}

/// List row inside a [`list_container`]: hairline top divider, comfortable
/// padding, hover wash for clickable rows.
pub fn list_row(palette: &theme::Palette, hoverable: bool) -> Div {
    let row = div()
        .flex()
        .gap_3()
        .items_center()
        .text_sm()
        .px_3()
        .py_2()
        .border_t_1()
        .border_color(palette.separator);
    if hoverable {
        row.hover(move |this| this.bg(palette.hover))
    } else {
        row
    }
}

/// Error box: destructive fill with foreground text.
pub fn error_box(err: &str, palette: &theme::Palette) -> impl IntoElement {
    div()
        .p_3()
        .rounded_md()
        .bg(palette.destructive)
        .text_color(palette.destructive_foreground)
        .child(format!("Error: {err}"))
}

/// Muted empty-state line for empty lists.
pub fn empty_state(text: impl IntoElement, palette: &theme::Palette) -> impl IntoElement {
    div()
        .py_6()
        .text_center()
        .text_sm()
        .text_color(palette.muted_foreground)
        .child(text)
}

/// Monospace span for IDs, timestamps, counts (JetBrains Mono + CJK mono).
pub fn mono_span(text: impl IntoElement) -> impl IntoElement {
    let m = theme::text::mono();
    div()
        .text_size(m.size)
        .font(theme::fonts::mono())
        .child(text)
}

/// Deterministic generated book cover: muted duotone derived from `seed`
/// (project id), serif initial, subtle spine edge. Pure element (no images,
/// no I/O) — renders instantly and never touches the render path with work.
///
/// `banner` selects a variant once persistent banners exist (`cover` =
/// EPUB cover via image pipeline, `gradient:<n>`, `none`); unknown values
/// fall back to the hash variant so old data never breaks rendering.
pub fn book_cover(
    title: &str,
    seed: &str,
    banner: Option<&str>,
    palette: &theme::Palette,
) -> impl IntoElement {
    // 8 muted duotone pairs (bg, ink): desaturated, dark-safe, distinct.
    const VARIANTS: [(u32, u32); 8] = [
        (0x2e3b3b, 0xd8e2dc), // sage
        (0x3b3340, 0xe0d6e8), // plum
        (0x333c44, 0xd3dce6), // slate
        (0x3f3a2e, 0xe6dcc8), // sand
        (0x2f3a33, 0xd2e2d4), // moss
        (0x3a2f35, 0xe2d2da), // mauve
        (0x2e3542, 0xd2dcea), // ink
        (0x3d332b, 0xe8d9c4), // clay
    ];
    let variant = match banner {
        Some(b) if b.starts_with("gradient:") => {
            b["gradient:".len()..].parse::<usize>().unwrap_or(0) % VARIANTS.len()
        }
        Some("none") => usize::MAX,
        _ => {
            // FNV-1a over the seed: stable across runs, no dependencies.
            let mut hash: u64 = 0xcbf29ce484222325;
            for byte in seed.bytes() {
                hash ^= byte as u64;
                hash = hash.wrapping_mul(0x100000001b3);
            }
            hash as usize % VARIANTS.len()
        }
    };
    let initial = title
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "W".to_string());
    if variant == usize::MAX {
        return div()
            .w(px(48.0))
            .h(px(68.0))
            .flex_none()
            .rounded_sm()
            .border_1()
            .border_color(palette.border)
            .bg(palette.muted)
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(20.0))
            .font(theme::fonts::serif())
            .text_color(palette.muted_foreground)
            .child(initial)
            .into_any_element();
    }
    let (bg, ink) = VARIANTS[variant];
    let bg = gpui_kit::Rgba {
        r: ((bg >> 16) & 0xff) as f32 / 255.0,
        g: ((bg >> 8) & 0xff) as f32 / 255.0,
        b: (bg & 0xff) as f32 / 255.0,
        a: 1.0,
    };
    let ink = gpui_kit::Rgba {
        r: ((ink >> 16) & 0xff) as f32 / 255.0,
        g: ((ink >> 8) & 0xff) as f32 / 255.0,
        b: (ink & 0xff) as f32 / 255.0,
        a: 1.0,
    };
    div()
        .w(px(48.0))
        .h(px(68.0))
        .flex_none()
        .rounded_sm()
        .overflow_hidden()
        .flex()
        .flex_row()
        .bg(gpui_kit::Hsla::from(bg))
        // Spine edge: darkened strip on the left (bg at 55% lightness).
        .child(
            div()
                .w(px(5.0))
                .h_full()
                .bg({
                    let bg_hsla = gpui_kit::Hsla::from(bg);
                    gpui_kit::Hsla { l: bg_hsla.l * 0.55, ..bg_hsla }
                })
                .flex_none(),
        )
        .child(
            div()
                .flex_1()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(22.0))
                .font(theme::fonts::serif())
                .text_color(gpui_kit::Hsla::from(ink))
                .child(initial),
        )
        .into_any_element()
}
