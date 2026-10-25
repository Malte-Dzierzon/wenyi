//! Navigation tree — port of `routes/manifest.tsx` navEntries.
//!
//! Global entries (always visible) + project entries (audience-filtered by
//! project format). Order, icons (lucide names for reference), and visibility
//! rules mirror the source exactly.

use crate::client::Project;

/// Audience rule — mirrors `isVisibleFor` in `manifest.tsx:103`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    All,
    Book,
    Srt,
}

impl Audience {
    pub fn visible_for(self, fmt: Option<&str>) -> bool {
        match self {
            Audience::All => true,
            Audience::Book => fmt.map(|f| f != "srt").unwrap_or(false),
            Audience::Srt => fmt == Some("srt"),
        }
    }
}

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

    /// Lucide icon name in the source (for visual reference).
    pub fn icon(self) -> &'static str {
        match self {
            GlobalNav::Projects => "LayoutDashboard",
            GlobalNav::CreateProject => "FolderPlus",
            GlobalNav::InterfaceSettings => "Settings",
        }
    }

    pub fn all() -> [GlobalNav; 3] {
        [
            GlobalNav::Projects,
            GlobalNav::CreateProject,
            GlobalNav::InterfaceSettings,
        ]
    }

    /// Entries for the sidebar `panel` slot (mirrors manifest `slot`).
    pub fn panel() -> [GlobalNav; 1] {
        [GlobalNav::Projects]
    }
}

/// Project nav entry — mirrors the 10 project navEntries in manifest order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectNav {
    Overview,
    Proofreading,
    Review,
    Subtitles,
    Glossary,
    Style,
    Contents,
    Export,
    Settings,
    Events,
}

impl ProjectNav {
    pub fn label(self) -> &'static str {
        match self {
            ProjectNav::Overview => "Translation overview",
            ProjectNav::Proofreading => "Manual proofreading",
            ProjectNav::Review => "Whole-book review",
            ProjectNav::Subtitles => "Subtitle editor",
            ProjectNav::Glossary => "Glossary",
            ProjectNav::Style => "Style & synopsis",
            ProjectNav::Contents => "Contents",
            ProjectNav::Export => "Export",
            ProjectNav::Settings => "Project settings",
            ProjectNav::Events => "Event log",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            ProjectNav::Overview => "Sparkles",
            ProjectNav::Proofreading => "BookOpenCheck",
            ProjectNav::Review => "ListChecks",
            ProjectNav::Subtitles => "Captions",
            ProjectNav::Glossary => "Library",
            ProjectNav::Style => "Languages",
            ProjectNav::Contents => "ListTree",
            ProjectNav::Export => "FileOutput",
            ProjectNav::Settings => "Settings2",
            ProjectNav::Events => "ScrollText",
        }
    }

    pub fn audience(self) -> Audience {
        match self {
            ProjectNav::Overview | ProjectNav::Export | ProjectNav::Settings | ProjectNav::Events => {
                Audience::All
            }
            ProjectNav::Subtitles => Audience::Srt,
            _ => Audience::Book,
        }
    }

    pub fn all() -> [ProjectNav; 10] {
        [
            ProjectNav::Overview,
            ProjectNav::Proofreading,
            ProjectNav::Review,
            ProjectNav::Subtitles,
            ProjectNav::Glossary,
            ProjectNav::Style,
            ProjectNav::Contents,
            ProjectNav::Export,
            ProjectNav::Settings,
            ProjectNav::Events,
        ]
    }

    /// Entries visible for the given project — mirrors `ProjectNavigation` filter.
    pub fn visible_for(project: &Project) -> Vec<ProjectNav> {
        Self::all()
            .into_iter()
            .filter(|n| n.audience().visible_for(project.fmt.as_deref()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_with_fmt(fmt: Option<&str>) -> Project {
        Project {
            id: "x".into(),
            name: "x".into(),
            title: None,
            fmt: fmt.map(str::to_string),
            source_lang: None,
            target_lang: None,
            status: "created".into(),
            error: None,
            initialized: false,
            created_at: None,
        }
    }

    #[test]
    fn book_hides_subtitles() {
        let visible = ProjectNav::visible_for(&project_with_fmt(Some("epub")));
        assert!(!visible.contains(&ProjectNav::Subtitles));
        assert!(visible.contains(&ProjectNav::Glossary));
        assert_eq!(visible.len(), 9);
    }

    #[test]
    fn srt_shows_only_srt_and_all() {
        let visible = ProjectNav::visible_for(&project_with_fmt(Some("srt")));
        assert!(visible.contains(&ProjectNav::Subtitles));
        assert!(!visible.contains(&ProjectNav::Glossary));
        assert!(!visible.contains(&ProjectNav::Proofreading));
        assert_eq!(visible.len(), 5);
    }

    #[test]
    fn order_matches_manifest() {
        let labels: Vec<_> = ProjectNav::all().iter().map(|n| n.label()).collect();
        assert_eq!(
            labels,
            [
                "Translation overview",
                "Manual proofreading",
                "Whole-book review",
                "Subtitle editor",
                "Glossary",
                "Style & synopsis",
                "Contents",
                "Export",
                "Project settings",
                "Event log",
            ]
        );
    }
}
