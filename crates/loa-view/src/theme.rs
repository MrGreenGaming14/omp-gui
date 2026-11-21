//! Appearance: the saved theme choice plus token overrides, as the UI reads and edits it.

/// A theme by registry name or file path; `system`, `dark` and `light` are registry names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeRef(pub String);

impl ThemeRef {
    pub const SYSTEM: &str = "system";

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ThemeRef {
    fn default() -> Self {
        Self(Self::SYSTEM.to_owned())
    }
}

/// The UI theme plus token overrides (a TOML table of dotted token keys).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Appearance {
    pub theme: ThemeRef,
    pub overrides: toml::Table,
}

/// An icon the UI draws; the kit maps each to its SVG.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Icon {
    ArrowUp,
    Bot,
    Brain,
    Check,
    ChevronDown,
    ChevronLeft,
    ChevronRight,
    ChevronUp,
    CircleDot,
    Cpu,
    ExternalLink,
    FileText,
    Files,
    Folder,
    Gauge,
    GitCompare,
    GitFork,
    Globe,
    Image,
    Maximize,
    Minimize,
    Monitor,
    PanelRight,
    PanelRightClose,
    Pin,
    Plus,
    Search,
    Settings,
    Shield,
    SlidersHorizontal,
    Sparkles,
    Square,
    Terminal,
    TriangleAlert,
    Wrench,
    X,
}
