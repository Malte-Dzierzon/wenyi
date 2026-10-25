//! Sidebar preferences — collapse state and resizable width.
//!
//! Ports `wenyi.sidebarCollapsed` (`AppLayout.tsx`) to a local JSON file,
//! mirroring the theme-choice persistence pattern in `theme.rs`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Sidebar sizing bounds (px) — mirrors the web shell (`md:w-60` expanded,
/// `md:w-16` collapsed) plus drag limits.
pub const EXPANDED_DEFAULT: f32 = 240.0;
pub const EXPANDED_MIN: f32 = 180.0;
pub const EXPANDED_MAX: f32 = 420.0;
pub const COLLAPSED_WIDTH: f32 = 64.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SidebarPrefs {
    #[serde(default)]
    pub collapsed: bool,
    #[serde(default = "default_width")]
    pub width: f32,
}

fn default_width() -> f32 {
    EXPANDED_DEFAULT
}

impl Default for SidebarPrefs {
    fn default() -> Self {
        Self {
            collapsed: false,
            width: EXPANDED_DEFAULT,
        }
    }
}

impl SidebarPrefs {
    /// Expanded width clamped to the drag bounds.
    pub fn clamped_width(&self) -> f32 {
        self.width.clamp(EXPANDED_MIN, EXPANDED_MAX)
    }
}

/// Prefs file location: `$WENYI_CONFIG_DIR/sidebar.json`, else
/// `$XDG_CONFIG_HOME/wenyi/sidebar.json`, else `~/.config/wenyi/sidebar.json`
/// (`%APPDATA%\wenyi\sidebar.json` on Windows), else `./wenyi-sidebar.json`.
pub fn prefs_file_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("WENYI_CONFIG_DIR") {
        return PathBuf::from(dir).join("sidebar.json");
    }
    #[cfg(windows)]
    if let Some(dir) = std::env::var_os("APPDATA") {
        return PathBuf::from(dir).join("wenyi").join("sidebar.json");
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("wenyi").join("sidebar.json");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("wenyi")
            .join("sidebar.json");
    }
    PathBuf::from("wenyi-sidebar.json")
}

/// Load the persisted prefs; returns [`SidebarPrefs::default`] when the file
/// is missing or unparsable (first run / hand-edited config).
pub fn load_prefs() -> SidebarPrefs {
    std::fs::read_to_string(prefs_file_path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Persist the prefs, creating the config dir as needed.
pub fn save_prefs(prefs: &SidebarPrefs) -> std::io::Result<()> {
    let path = prefs_file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let raw = serde_json::to_string_pretty(prefs)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, raw)
}

/// Clamp a dragged expanded width into [`EXPANDED_MIN`]..=[`EXPANDED_MAX`].
pub fn clamp_width(width: f32) -> f32 {
    width.clamp(EXPANDED_MIN, EXPANDED_MAX)
}
