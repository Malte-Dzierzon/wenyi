//! Wenyi design tokens — port of `packages/ui/src/index.css` + `tailwind.preset.js`.
//!
//! shadcn-style HSL triplets, light (`:root`) + dark (`.dark`).
//! Spacing: px scale (`key * 4px`). Radius `--radius: 8px` (see [`radius`]).
//!
//! Zed-inspired extensions (all additive — [`Palette::light`]/[`Palette::dark`],
//! [`space`] and [`radius`] keep stable signatures):
//! - [`Palette`] carries semantic roles: selection, hover wash, focus ring,
//!   badge tones (success/warning/info + on-fill foregrounds), a [`ChartPalette`],
//!   hairline [`Palette::separator`], modal [`Palette::overlay`] scrim, and an
//!   accent-tinted [`Palette::accent_soft`] wash for active nav/selected rows.
//! - [`text`] holds the typography scale, [`icon`] the icon sizes,
//!   [`layout`] a few shell constants, [`radius`] stays Zed-small (6px default).
//! - [`Accent`] (blue/violet/green/amber) + [`ThemeChoice`] select mode+accent;
//!   [`Palette::with_accent`] derives a coherent palette from a choice.
//! - [`load_choice`]/[`save_choice`] persist the choice as JSON (App wiring is
//!   the lead's job); [`system_is_dark`] reads the OS appearance via
//!   `gpui_kit::App::window_appearance`.

use gpui_kit::{FontWeight, Hsla, Pixels, hsla, px};
use serde::{Deserialize, Serialize};
use std::{env, path::PathBuf, sync::{atomic::{AtomicBool, Ordering}, LazyLock, Mutex}};

/// Convert an HSL triplet (h in degrees, s/l in 0..1) to GPUI Hsla.
fn hsl(h: f32, s: f32, l: f32) -> Hsla {
    hsla(h / 360.0, s, l, 1.0)
}

/// Convert an HSL triplet + alpha (h in degrees) to GPUI Hsla.
fn hsla_deg(h: f32, s: f32, l: f32, a: f32) -> Hsla {
    hsla(h / 360.0, s, l, a)
}

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    // --- shadcn base roles (stable; values match index.css) ---
    pub background: Hsla,
    pub foreground: Hsla,
    pub card: Hsla,
    pub card_foreground: Hsla,
    pub primary: Hsla,
    pub primary_foreground: Hsla,
    pub secondary: Hsla,
    pub secondary_foreground: Hsla,
    pub muted: Hsla,
    pub muted_foreground: Hsla,
    pub accent: Hsla,
    pub accent_foreground: Hsla,
    pub destructive: Hsla,
    pub destructive_foreground: Hsla,
    pub border: Hsla,
    pub input: Hsla,
    pub ring: Hsla,
    // --- Zed-style semantic roles ---
    /// Selected-row / selected-item fill (accent-tinted wash).
    pub selection: Hsla,
    /// Hover wash for rows, list items, cards.
    pub hover: Hsla,
    /// Keyboard-focus outline (accent-derived, brighter than `ring` on dark).
    pub focus_ring: Hsla,
    /// Success badge/dot/large-text tone.
    pub success: Hsla,
    /// Text placed on a `success` fill.
    pub success_foreground: Hsla,
    /// Warning badge/dot/large-text tone.
    pub warning: Hsla,
    /// Text placed on a `warning` fill.
    pub warning_foreground: Hsla,
    /// Info badge/dot/large-text tone.
    pub info: Hsla,
    /// Text placed on an `info` fill.
    pub info_foreground: Hsla,
    /// Hairline divider, subtler than `border`.
    pub separator: Hsla,
    /// Scrim behind modals/popovers.
    pub overlay: Hsla,
    /// Soft accent wash for active nav items / toggled controls.
    pub accent_soft: Hsla,
    /// Categorical series colors for charts/ledgers.
    pub chart: ChartPalette,
}

/// Categorical series colors for progress/report charts.
///
/// Index wraps (`get(i)`), so any series count works. Dark variants are
/// brightened ~10-15 lightness points so thin strokes read on near-black.
#[derive(Debug, Clone, Copy)]
pub struct ChartPalette {
    pub series: [Hsla; 6],
}

impl ChartPalette {
    fn light() -> Self {
        Self {
            series: [
                hsl(217.0, 0.91, 0.60), // blue
                hsl(142.0, 0.71, 0.45), // green
                hsl(32.0, 0.95, 0.48),  // amber
                hsl(262.0, 0.83, 0.58), // violet
                hsl(346.0, 0.77, 0.55), // rose
                hsl(173.0, 0.80, 0.40), // teal
            ],
        }
    }

    fn dark() -> Self {
        Self {
            series: [
                hsl(213.0, 0.94, 0.68), // blue
                hsl(142.0, 0.69, 0.58), // green
                hsl(38.0, 0.95, 0.62),  // amber
                hsl(258.0, 0.90, 0.72), // violet
                hsl(350.0, 0.85, 0.68), // rose
                hsl(172.0, 0.75, 0.55), // teal
            ],
        }
    }

    /// Series color with wrapping index.
    pub fn get(&self, index: usize) -> Hsla {
        self.series[index % self.series.len()]
    }
}

impl Palette {
    /// Light theme — `index.css :root`.
    pub fn light() -> Self {
        Self {
            background: hsl(0.0, 0.0, 1.0),
            foreground: hsl(240.0, 0.10, 0.039),
            card: hsl(0.0, 0.0, 1.0),
            card_foreground: hsl(240.0, 0.10, 0.039),
            primary: hsl(240.0, 0.059, 0.10),
            primary_foreground: hsl(0.0, 0.0, 0.98),
            secondary: hsl(240.0, 0.048, 0.959),
            secondary_foreground: hsl(240.0, 0.059, 0.10),
            muted: hsl(240.0, 0.048, 0.959),
            muted_foreground: hsl(240.0, 0.038, 0.461),
            accent: hsl(240.0, 0.048, 0.959),
            accent_foreground: hsl(240.0, 0.059, 0.10),
            destructive: hsl(0.0, 0.842, 0.602),
            destructive_foreground: hsl(0.0, 0.0, 0.98),
            border: hsl(240.0, 0.059, 0.90),
            input: hsl(240.0, 0.059, 0.90),
            ring: hsl(240.0, 0.059, 0.10),
            selection: hsl(213.0, 0.96, 0.90),
            hover: hsl(240.0, 0.048, 0.94),
            focus_ring: hsl(217.0, 0.91, 0.60),
            success: hsl(142.0, 0.76, 0.36),
            success_foreground: hsl(0.0, 0.0, 0.98),
            warning: hsl(32.0, 0.95, 0.44),
            warning_foreground: hsl(28.0, 1.0, 0.10),
            info: hsl(221.0, 0.83, 0.53),
            info_foreground: hsl(0.0, 0.0, 0.98),
            separator: hsl(240.0, 0.048, 0.92),
            overlay: hsla_deg(0.0, 0.0, 0.0, 0.5),
            accent_soft: hsl(214.0, 0.95, 0.93),
            chart: ChartPalette::light(),
        }
    }

    /// Dark theme — `index.css .dark`, audited for contrast (see notes below).
    ///
    /// Dark-audit changes vs the raw `.dark` port, with rationale:
    /// - `card` lifted to l=0.055 (was identical to `background`): panels and
    ///   cards need one step of elevation to read on the near-black app bg,
    ///   Zed-style layered surfaces.
    /// - `muted_foreground` raised to l=0.72 (was 0.649, ~4.3:1 on the bg):
    ///   secondary/small text now clears ~5.5:1.
    /// - `border`/`input` raised to l=0.22 (were 0.159, identical to the
    ///   secondary/muted/accent surfaces): hairline borders were invisible;
    ///   inputs now read as raised fields and cards get definition.
    /// - `destructive` brightened to hsl(0, 72%, 46%) (was 62.8%/30.6%, a
    ///   near-black red indistinguishable from the bg): error fills now read
    ///   as red while keeping white-text contrast.
    /// - New roles follow the standard dark-mode pattern: status tones and
    ///   the focus ring are brightened for use as dots/text/strokes on dark,
    ///   while each `*_foreground` is a dark ink for text placed *on* the
    ///   tone fill. `selection`/`accent_soft` are translucent accent washes so
    ///   they tint any underlying surface instead of flattening it.
    pub fn dark() -> Self {
        Self {
            background: hsl(240.0, 0.10, 0.039),
            foreground: hsl(0.0, 0.0, 0.98),
            card: hsl(240.0, 0.09, 0.055),
            card_foreground: hsl(0.0, 0.0, 0.98),
            primary: hsl(0.0, 0.0, 0.98),
            primary_foreground: hsl(240.0, 0.059, 0.10),
            secondary: hsl(240.0, 0.037, 0.159),
            secondary_foreground: hsl(0.0, 0.0, 0.98),
            muted: hsl(240.0, 0.037, 0.159),
            muted_foreground: hsl(240.0, 0.05, 0.72),
            accent: hsl(240.0, 0.037, 0.159),
            accent_foreground: hsl(0.0, 0.0, 0.98),
            destructive: hsl(0.0, 0.72, 0.46),
            destructive_foreground: hsl(0.0, 0.0, 0.98),
            border: hsl(240.0, 0.037, 0.22),
            input: hsl(240.0, 0.037, 0.22),
            ring: hsl(240.0, 0.049, 0.839),
            selection: hsla_deg(217.0, 0.90, 0.60, 0.28),
            hover: hsl(240.0, 0.037, 0.19),
            focus_ring: hsl(213.0, 0.94, 0.68),
            success: hsl(142.0, 0.69, 0.58),
            success_foreground: hsl(140.0, 0.60, 0.10),
            warning: hsl(38.0, 0.95, 0.60),
            warning_foreground: hsl(30.0, 0.90, 0.12),
            info: hsl(213.0, 0.94, 0.68),
            info_foreground: hsl(222.0, 0.60, 0.12),
            separator: hsl(240.0, 0.037, 0.12),
            overlay: hsla_deg(0.0, 0.0, 0.0, 0.6),
            accent_soft: hsla_deg(217.0, 0.90, 0.65, 0.16),
            chart: ChartPalette::dark(),
        }
    }

    /// Derive a palette for `dark` with `accent` applied coherently.
    ///
    /// Dark + Noctalia: overrides `primary`, `primary_foreground`, `ring`,
    /// `focus_ring`, `selection`, and `accent_soft`.
    /// Light: keeps the original `:root` neutrals (`primary` near-black with
    /// white text, `ring` to match) and derives only `focus_ring`,
    /// `selection`, and `accent_soft` from the accent — the accent tints
    /// focus/selection, it does not recolor primary controls.
    /// The neutral shadcn surfaces (`accent`, `secondary`, `muted`, …) are
    /// intentionally left alone so existing views keep their neutral chrome
    /// regardless of accent.
    pub fn with_accent(dark: bool, accent: Accent) -> Self {
        let mut palette = if dark { Self::dark() } else { Self::light() };
        if dark {
            palette.primary = accent.base(true);
            palette.primary_foreground = accent.on(true);
            palette.ring = accent.base(true);
        }
        palette.focus_ring = accent.base(dark);
        palette.selection = accent.soft(dark);
        palette.accent_soft = accent.soft(dark);
        palette
    }
    /// Noctalia-derived palette (reads ~/.config/noctalia/palettes/*.json;
    /// falls back to built-in dark when missing/unparsable).
    pub fn noctalia() -> Self {
        load_noctalia_palette().unwrap_or_else(Self::dark)
    }
}

/// Selectable accent preset.
///
/// Light bases sit at ~4:1+ against white `on` text (violet/green clear 5:1;
/// blue ~4.2:1 for large/bold control text); dark bases are brightened for use
/// on near-black and pair with a dark `on` ink (~7:1). Amber always pairs with
/// a dark-brown ink — white text on amber never reaches body-text contrast.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Accent {
    Blue,
    Violet,
    Green,
    Amber,
}

impl Accent {
    /// All presets, in settings-display order.
    pub fn all() -> [Accent; 4] {
        [Accent::Blue, Accent::Violet, Accent::Green, Accent::Amber]
    }

    /// Display name for settings UI.
    pub fn name(&self) -> &'static str {
        match self {
            Accent::Blue => "Blue",
            Accent::Violet => "Violet",
            Accent::Green => "Green",
            Accent::Amber => "Amber",
        }
    }

    /// Solid accent color (buttons, toggles, focus).
    pub fn base(&self, dark: bool) -> Hsla {
        match (self, dark) {
            (Accent::Blue, false) => hsl(217.0, 0.91, 0.55),
            (Accent::Blue, true) => hsl(213.0, 0.94, 0.68),
            (Accent::Violet, false) => hsl(263.0, 0.70, 0.55),
            (Accent::Violet, true) => hsl(258.0, 0.90, 0.72),
            (Accent::Green, false) => hsl(142.0, 0.76, 0.33),
            (Accent::Green, true) => hsl(142.0, 0.69, 0.55),
            (Accent::Amber, false) => hsl(36.0, 1.0, 0.50),
            (Accent::Amber, true) => hsl(42.0, 0.96, 0.62),
        }
    }

    /// Text/icon ink placed on [`Accent::base`].
    pub fn on(&self, dark: bool) -> Hsla {
        match (self, dark) {
            (Accent::Blue, false) => hsl(0.0, 0.0, 0.98),
            (Accent::Blue, true) => hsl(222.0, 0.60, 0.12),
            (Accent::Violet, false) => hsl(0.0, 0.0, 0.98),
            (Accent::Violet, true) => hsl(255.0, 0.60, 0.12),
            (Accent::Green, false) => hsl(0.0, 0.0, 0.98),
            (Accent::Green, true) => hsl(140.0, 0.60, 0.10),
            (Accent::Amber, _) => hsl(30.0, 0.90, 0.12),
        }
    }

    /// Translucent wash for selections / active nav items.
    pub fn soft(&self, dark: bool) -> Hsla {
        if dark {
            let base = self.base(true);
            Hsla { a: 0.22, ..base }
        } else {
            match self {
                Accent::Blue => hsl(214.0, 0.95, 0.93),
                Accent::Violet => hsl(250.0, 0.95, 0.94),
                Accent::Green => hsl(141.0, 0.80, 0.92),
                Accent::Amber => hsl(45.0, 1.0, 0.90),
            }
        }
    }
}

/// Light / dark / follow-the-OS / Noctalia appearance mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ThemeMode {
    Light,
    Dark,
    #[default]
    System,
    Noctalia,
}

impl ThemeMode {
    /// Resolve to a concrete dark flag given the OS appearance.
    pub fn resolve_dark(&self, system_dark: bool) -> bool {
        match self {
            ThemeMode::Light => false,
            ThemeMode::Dark => true,
            ThemeMode::System => system_dark,
            ThemeMode::Noctalia => true,
        }
    }
}

/// Persisted theme selection: appearance mode + accent preset.
///
/// App wiring (reading this at startup, applying [`Palette::with_accent`],
/// re-resolving on OS appearance change) is the lead's job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeChoice {
    pub mode: ThemeMode,
    pub accent: Accent,
}

impl Default for ThemeChoice {
    /// Light-first default: the original web design is light (`:root`);
    /// dark/Noctalia remain available in Interface settings.
    fn default() -> Self {
        Self { mode: ThemeMode::Light, accent: Accent::Blue }
    }
}
impl ThemeChoice {
    /// Concrete palette for this choice given the OS appearance.
    pub fn palette(&self, system_dark: bool) -> Palette {
        match self.mode {
            ThemeMode::Noctalia => Palette::noctalia(),
            _ => Palette::with_accent(self.mode.resolve_dark(system_dark), self.accent),
        }
    }

    /// Concrete palette for this choice using the live OS appearance.
    pub fn palette_live(&self, cx: &gpui_kit::App) -> Palette {
        self.palette(system_is_dark(cx))
    }
}
/// Config file location: `$WENYI_CONFIG_DIR/theme.json`, else
/// `$XDG_CONFIG_HOME/wenyi/theme.json`, else `~/.config/wenyi/theme.json`
/// (`%APPDATA%\wenyi\theme.json` on Windows), else `./wenyi-theme.json`.
pub fn choice_file_path() -> PathBuf {
    if let Some(dir) = env::var_os("WENYI_CONFIG_DIR") {
        return PathBuf::from(dir).join("theme.json");
    }
    #[cfg(windows)]
    if let Some(dir) = env::var_os("APPDATA") {
        return PathBuf::from(dir).join("wenyi").join("theme.json");
    }
    if let Some(xdg) = env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("wenyi").join("theme.json");
    }
    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home).join(".config").join("wenyi").join("theme.json");
    }
    PathBuf::from("wenyi-theme.json")
}

/// Load the persisted choice; returns [`ThemeChoice::default`] when the file
/// is missing or unparsable (first run / hand-edited config).
pub fn load_choice() -> ThemeChoice {
    std::fs::read_to_string(choice_file_path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Persist the choice, creating the config dir as needed.
pub fn save_choice(choice: &ThemeChoice) -> std::io::Result<()> {
    let path = choice_file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_string_pretty(choice)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, raw)
}

/// Report whether the OS currently uses a dark appearance.
///
/// Backed by `gpui_kit::App::window_appearance` (works on macOS/Windows/Linux
/// via the xdg portal); no fallback needed — unknown appearances report light.
pub fn system_is_dark(cx: &gpui_kit::App) -> bool {
    use gpui_kit::WindowAppearance as Appearance;
    matches!(cx.window_appearance(), Appearance::Dark | Appearance::VibrantDark)
}

/// Convenience for App wiring: resolve + build the palette in one call.
pub fn palette_for(choice: &ThemeChoice, cx: &gpui_kit::App) -> Palette {
    choice.palette_live(cx)
}

// ---------- Global (process-wide) theme state ----------
// Views are independent entities; they read the current choice through this
// small global instead of threading a palette through every render path.
static THEME_CHOICE: LazyLock<Mutex<ThemeChoice>> = LazyLock::new(|| Mutex::new(load_choice()));
static SYSTEM_DARK: AtomicBool = AtomicBool::new(false);

fn theme_choice_cell() -> &'static Mutex<ThemeChoice> {
    &THEME_CHOICE
}

pub fn set_choice(choice: ThemeChoice) {
    *theme_choice_cell().lock().unwrap() = choice;
}

pub fn current_choice() -> ThemeChoice {
    *theme_choice_cell().lock().unwrap()
}

pub fn set_system_dark(dark: bool) {
    SYSTEM_DARK.store(dark, Ordering::Relaxed);
}

pub fn current_system_dark() -> bool {
    SYSTEM_DARK.load(Ordering::Relaxed)
}

/// Convenience: palette from the global choice + OS appearance.
pub fn current_palette() -> Palette {
    current_choice().palette(current_system_dark())
}

// ---------- Noctalia palette loading ----------
#[derive(Deserialize, Default)]
struct NoctaliaTerminal {
    #[serde(default)]
    normal: Vec<String>,
    #[serde(default)]
    bright: Vec<String>,
}

#[derive(Deserialize, Default)]
struct NoctaliaDark {
    #[serde(rename = "mPrimary")]
    m_primary: Option<String>,
    #[serde(rename = "mSecondary")]
    m_secondary: Option<String>,
    #[serde(rename = "mTertiary")]
    m_tertiary: Option<String>,
    #[serde(rename = "mSurface")]
    m_surface: Option<String>,
    #[serde(rename = "mSurfaceVariant")]
    m_surface_variant: Option<String>,
    #[serde(rename = "mError")]
    m_error: Option<String>,
    #[serde(rename = "mOutline")]
    m_outline: Option<String>,
    #[serde(rename = "mHover")]
    m_hover: Option<String>,
    #[serde(rename = "mShadow")]
    m_shadow: Option<String>,
    #[serde(rename = "mOnPrimary")]
    m_on_primary: Option<String>,
    #[serde(rename = "mOnSecondary")]
    m_on_secondary: Option<String>,
    #[serde(rename = "mOnSurface")]
    m_on_surface: Option<String>,
    #[serde(rename = "mOnSurfaceVariant")]
    m_on_surface_variant: Option<String>,
    #[serde(rename = "mOnTertiary")]
    m_on_tertiary: Option<String>,
    #[serde(rename = "mOnError")]
    m_on_error: Option<String>,
    #[serde(rename = "mOnHover")]
    m_on_hover: Option<String>,
    #[serde(default)]
    terminal: Option<NoctaliaTerminal>,
}

#[derive(Deserialize, Default)]
struct NoctaliaFile {
    #[serde(default)]
    dark: Option<NoctaliaDark>,
}

fn hex_to_hsla(s: &str) -> Option<Hsla> {
    let rgba = gpui_kit::Rgba::try_from(s).ok()?;
    Some(Hsla::from(rgba))
}

fn noctalia_palette_path() -> Option<PathBuf> {
    let home = env::var_os("HOME")?;
    let dir = PathBuf::from(home).join(".config").join("noctalia").join("palettes");
    let haven = dir.join("Haven.json");
    if haven.is_file() {
        return Some(haven);
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
        .collect();
    entries.sort();
    entries.into_iter().next()
}

pub fn load_noctalia_palette() -> Option<Palette> {
    let path = noctalia_palette_path()?;
    let raw = std::fs::read_to_string(&path).ok()?;
    let parsed: NoctaliaFile = serde_json::from_str(&raw).ok()?;
    let d = parsed.dark?;
    let surface = hex_to_hsla(d.m_surface.as_deref()?)?;
    let on_surface = hex_to_hsla(d.m_on_surface.as_deref()?)?;
    let primary = hex_to_hsla(d.m_primary.as_deref().unwrap_or("#888888"))?;
    let on_primary = d
        .m_on_primary
        .as_deref()
        .and_then(hex_to_hsla)
        .unwrap_or(surface);
    let surface_variant = d
        .m_surface_variant
        .as_deref()
        .and_then(hex_to_hsla)
        .unwrap_or(surface);
    let on_surface_variant = d
        .m_on_surface_variant
        .as_deref()
        .and_then(hex_to_hsla)
        .unwrap_or(on_surface);
    let outline = d
        .m_outline
        .as_deref()
        .and_then(hex_to_hsla)
        .unwrap_or(surface_variant);
    let error = d
        .m_error
        .as_deref()
        .and_then(hex_to_hsla)
        .unwrap_or_else(|| hsl(0.0, 0.7, 0.5));
    let on_error = d
        .m_on_error
        .as_deref()
        .and_then(hex_to_hsla)
        .unwrap_or(on_surface);
    let hover = d
        .m_hover
        .as_deref()
        .and_then(hex_to_hsla)
        .unwrap_or(surface_variant);
    let secondary = d
        .m_secondary
        .as_deref()
        .and_then(hex_to_hsla)
        .unwrap_or(primary);
    let tertiary = d
        .m_tertiary
        .as_deref()
        .and_then(hex_to_hsla)
        .unwrap_or(secondary);
    let on_secondary = d
        .m_on_secondary
        .as_deref()
        .and_then(hex_to_hsla)
        .unwrap_or(on_surface);
    let on_tertiary = d
        .m_on_tertiary
        .as_deref()
        .and_then(hex_to_hsla)
        .unwrap_or(on_surface);
    let on_hover = d
        .m_on_hover
        .as_deref()
        .and_then(hex_to_hsla)
        .unwrap_or(on_surface);

    // terminal colors for badge tones / chart series
    let term = d.terminal.as_ref();
    let normal = term.map(|t| t.normal.as_slice()).unwrap_or(&[]);
    let bright = term.map(|t| t.bright.as_slice()).unwrap_or(&[]);
    let tn = |i: usize, fallback: Hsla| -> Hsla {
        normal.get(i).and_then(|s| hex_to_hsla(s)).unwrap_or(fallback)
    };
    let tb = |i: usize, fallback: Hsla| -> Hsla {
        bright.get(i).and_then(|s| hex_to_hsla(s)).unwrap_or(fallback)
    };
    // conventional terminal order: black, red, green, yellow, blue, magenta, cyan, white
    let success = tn(2, hsl(142.0, 0.7, 0.45));
    let warning = tn(3, hsl(45.0, 0.9, 0.5));
    let info = tn(4, hsl(217.0, 0.9, 0.6));
    let err_chart = tn(1, error);

    let chart = ChartPalette {
        series: [
            info,
            success,
            warning,
            tertiary,
            err_chart,
            tb(6, hsl(173.0, 0.8, 0.4)),
        ],
    };

    Some(Palette {
        background: surface,
        foreground: on_surface,
        card: surface_variant,
        card_foreground: on_surface,
        primary,
        primary_foreground: on_primary,
        secondary: surface_variant,
        secondary_foreground: on_surface_variant,
        muted: surface_variant,
        muted_foreground: on_surface_variant,
        accent: secondary,
        accent_foreground: on_secondary,
        destructive: error,
        destructive_foreground: on_error,
        border: outline,
        input: outline,
        ring: primary,
        selection: hover,
        hover,
        focus_ring: primary,
        success,
        success_foreground: on_surface,
        warning,
        warning_foreground: on_surface,
        info,
        info_foreground: on_surface,
        separator: outline,
        overlay: hex_to_hsla(d.m_shadow.as_deref().unwrap_or("#000000")).unwrap_or(hsl(0.0, 0.0, 0.0)),
        accent_soft: hover,
        chart,
    })
}
/// A sized + weighted text token from the typography scale.
///
/// Zed-like compact scale: 20px view title down to 12px captions/mono.
/// Pair with `.text_color(...)`; apply via `.text_size(t.size)` +
/// `.font_weight(t.weight)`.
#[derive(Debug, Clone, Copy)]
pub struct TypeToken {
    pub size: Pixels,
    pub weight: FontWeight,
}

/// Typography scale.
pub mod text {
    use super::TypeToken;
    use gpui_kit::{FontWeight, px};

    fn token(size: f32, weight: FontWeight) -> TypeToken {
        TypeToken { size: px(size), weight }
    }

    /// View title (current views use `.text_xl()` ≈ 20px bold; prefer semibold).
    pub fn title() -> TypeToken {
        token(20.0, FontWeight::SEMIBOLD)
    }
    /// Section heading.
    pub fn h1() -> TypeToken {
        token(18.0, FontWeight::SEMIBOLD)
    }
    /// Sub-heading / card title.
    pub fn h2() -> TypeToken {
        token(16.0, FontWeight::SEMIBOLD)
    }
    /// Default body text.
    pub fn body() -> TypeToken {
        token(14.0, FontWeight::NORMAL)
    }
    /// Form labels, nav items.
    pub fn label() -> TypeToken {
        token(13.0, FontWeight::MEDIUM)
    }
    /// Hints, timestamps, table meta.
    pub fn caption() -> TypeToken {
        token(12.0, FontWeight::NORMAL)
    }
    /// Code / timings / JSON summaries (pair with a monospace family).
    pub fn mono() -> TypeToken {
        token(12.0, FontWeight::NORMAL)
    }
}

/// Icon sizes (Lucide, 1px stroke at these boxes).
pub mod icon {
    use gpui_kit::{Pixels, px};

    pub fn xs() -> Pixels {
        px(12.0)
    }
    pub fn sm() -> Pixels {
        px(14.0)
    }
    /// Default UI icon.
    pub fn md() -> Pixels {
        px(16.0)
    }
    pub fn lg() -> Pixels {
        px(20.0)
    }
    pub fn xl() -> Pixels {
        px(24.0)
    }
}

/// Shell layout constants (4px-base multiples).
pub mod layout {
    use gpui_kit::{Pixels, px};

    /// Sidebar width (Zed's is ~240-300 depending on density).
    pub fn sidebar_width() -> Pixels {
        px(248.0)
    }
    /// Max content column for settings/progress views.
    pub fn content_max_width() -> Pixels {
        px(960.0)
    }
    /// Standard list row height.
    pub fn row_height() -> Pixels {
        px(32.0)
    }
    /// Standard input/button height.
    pub fn input_height() -> Pixels {
        px(32.0)
    }
}

/// Spacing scale — `tailwind.preset.js` pxScale (`key * 4px`).
pub fn space(key: u32) -> Pixels {
    px((key * 4) as f32)
}

/// Radius scale — `--radius: 8px`; sm/md/lg per preset.
///
/// Zed-style default is 6px ([`radius::md`]) for controls/cards; keep `lg`/`xl`
/// for sheets and window chrome.
pub mod radius {
    use gpui_kit::{px, Pixels};

    pub fn xs() -> Pixels {
        px(3.0)
    }
    pub fn sm() -> Pixels {
        px(4.0)
    }
    /// Default control/card radius.
    pub fn md() -> Pixels {
        px(6.0)
    }
    pub fn lg() -> Pixels {
        px(8.0)
    }
    pub fn xl() -> Pixels {
        px(12.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accent_overrides_primary_and_wash() {
        let base = Palette::dark();
        let violet = Palette::with_accent(true, Accent::Violet);
        assert_eq!(violet.primary, Accent::Violet.base(true));
        assert_eq!(violet.primary_foreground, Accent::Violet.on(true));
        assert_eq!(violet.focus_ring, Accent::Violet.base(true));
        // Neutral surfaces are untouched by the accent.
        assert_eq!(violet.secondary, base.secondary);
        assert_eq!(violet.muted, base.muted);
        assert_eq!(violet.border, base.border);
    }

    #[test]
    fn choice_mode_resolution() {
        let dark = ThemeChoice { mode: ThemeMode::Dark, accent: Accent::Green };
        assert!(dark.mode.resolve_dark(false));
        let light = ThemeChoice { mode: ThemeMode::Light, accent: Accent::Green };
        assert!(!light.mode.resolve_dark(true));
        let sys = ThemeChoice { mode: ThemeMode::System, accent: Accent::Blue };
        assert!(sys.mode.resolve_dark(true));
        assert!(!sys.mode.resolve_dark(false));
        // Resolved palettes match the direct constructors' roles.
        assert_eq!(dark.palette(false).primary, Accent::Green.base(true));
        // Light keeps the original `:root` near-black primary (white text);
        // the accent only tints focus/selection washes.
        assert_eq!(light.palette(true).primary, Palette::light().primary);
        assert_eq!(light.palette(true).primary_foreground, Palette::light().primary_foreground);
        assert_eq!(light.palette(true).focus_ring, Accent::Green.base(false));
        assert_eq!(light.palette(true).selection, Accent::Green.soft(false));
    }

    #[test]
    fn choice_serde_round_trip() {
        let choice = ThemeChoice { mode: ThemeMode::Dark, accent: Accent::Amber };
        let raw = serde_json::to_string(&choice).unwrap();
        assert_eq!(serde_json::from_str::<ThemeChoice>(&raw).unwrap(), choice);
        // Corrupt config falls back to default instead of erroring.
        assert_eq!(
            serde_json::from_str::<ThemeChoice>("{nope").unwrap_or_default(),
            ThemeChoice::default()
        );
    }

    #[test]
    fn chart_index_wraps() {
        let chart = ChartPalette::dark();
        assert_eq!(chart.get(0), chart.series[0]);
        assert_eq!(chart.get(6), chart.series[0]);
        assert_eq!(chart.get(7), chart.series[1]);
    }

    #[test]
    fn choice_persistence_round_trip() {
        let key = "WENYI_CONFIG_DIR";
        let prev = env::var_os(key);
        let dir = env::temp_dir().join(format!("wenyi-theme-test-{}", std::process::id()));
        env::set_var(key, &dir);
        let choice = ThemeChoice { mode: ThemeMode::Dark, accent: Accent::Amber };
        let result = save_choice(&choice).map(|_| load_choice());
        let _ = std::fs::remove_dir_all(&dir);
        match prev {
            Some(v) => env::set_var(key, v),
            None => env::remove_var(key),
        }
        assert_eq!(result.unwrap(), choice);
    }
}
