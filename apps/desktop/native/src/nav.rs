//! Navigation tree — port of `routes/manifest.tsx` navEntries.
//!
//! Slice 7+: 6 project screens (Overview, Proofreading, Glossary, Style,
//! Export, Events) + 3 global screens. Dropped: Review, Subtitles, Contents
//! (folded into proofreading chapter list), per-project Settings.

/// Global nav entry — mirrors the 3 global navEntries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalNav {
    Projects,
    CreateProject,
    InterfaceSettings,
}

impl GlobalNav {
    pub fn label(self) -> &'static str {
        match self {
            GlobalNav::Projects => "Projects",
            GlobalNav::CreateProject => "Create project",
            GlobalNav::InterfaceSettings => "Interface settings",
        }
    }

    /// Entries for the sidebar `panel` slot (mirrors manifest `slot`).
    pub fn panel() -> [GlobalNav; 1] {
        [GlobalNav::Projects]
    }
}

/// Project nav entry — the 6 Slice 7+ screens in sidebar order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectNav {
    Overview,
    Proofreading,
    Glossary,
    Style,
    Export,
    Events,
}

impl ProjectNav {
    pub fn label(self) -> &'static str {
        match self {
            ProjectNav::Overview => "Translation overview",
            ProjectNav::Proofreading => "Manual proofreading",
            ProjectNav::Glossary => "Glossary",
            ProjectNav::Style => "Book knowledge",
            ProjectNav::Export => "Export",
            ProjectNav::Events => "Event log",
        }
    }
    pub fn all() -> [ProjectNav; 6] {
        [
            ProjectNav::Overview,
            ProjectNav::Proofreading,
            ProjectNav::Glossary,
            ProjectNav::Style,
            ProjectNav::Export,
            ProjectNav::Events,
        ]
    }

    /// Entries visible for the project — all six, format-independent.
    pub fn visible_for() -> Vec<ProjectNav> {
        Self::all().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_screens_visible() {
        assert_eq!(ProjectNav::visible_for().len(), 6);
    }

    #[test]
    fn order_matches_manifest() {
        let labels: Vec<_> = ProjectNav::all().iter().map(|n| n.label()).collect();
        assert_eq!(
            labels,
            [
                "Translation overview",
                "Manual proofreading",
                "Glossary",
                "Book knowledge",
                "Export",
                "Event log",
            ]
        );
    }
}
