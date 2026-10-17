//! Navigation model of the left rail, free of GPUI so it is unit tested in the default build.
//!
//! The rail lists the screens in [`Screen::ALL`] order with live counters ([`NavCounters`]).
//! Screens that are not built yet render a placeholder naming the story that owns them
//! ([`Screen::owner`]).

/// A top-level screen reachable from the rail (`design/README.md`, *Shell*).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Screen {
    /// Local models (screen 10).
    Library,
    /// Hugging Face and Ollama search and download (screen 11).
    Discover,
    /// Loaded models, hardware, live requests (screen 13).
    Running,
    /// Engine recipes and builds (screen 15).
    Engines,
    /// External access, keys, endpoints, HF token (screen 14).
    ApiKeys,
    /// Storage, model loading, server connection, theme, logging (screen 16).
    Settings,
}

impl Screen {
    /// Rail order.
    pub const ALL: [Self; 6] = [
        Self::Library,
        Self::Discover,
        Self::Running,
        Self::Engines,
        Self::ApiKeys,
        Self::Settings,
    ];

    /// Label in the rail and screen title.
    pub fn label(self) -> &'static str {
        match self {
            Self::Library => "Library",
            Self::Discover => "Discover",
            Self::Running => "Running",
            Self::Engines => "Engines",
            Self::ApiKeys => "API keys",
            Self::Settings => "Settings",
        }
    }

    /// Stable id for GPUI elements (`nav-library`).
    pub fn id(self) -> &'static str {
        match self {
            Self::Library => "nav-library",
            Self::Discover => "nav-discover",
            Self::Running => "nav-running",
            Self::Engines => "nav-engines",
            Self::ApiKeys => "nav-api-keys",
            Self::Settings => "nav-settings",
        }
    }

    /// The backlog story that builds the screen's content.
    pub fn owner(self) -> &'static str {
        match self {
            Self::Library => "ALK-US-0066",
            Self::Discover => "ALK-US-0067",
            Self::Running => "ALK-US-0070",
            Self::Engines => "ALK-US-0072",
            Self::ApiKeys => "ALK-US-0071",
            Self::Settings => "ALK-US-0081",
        }
    }
}

/// Live counters shown at the right of rail items. `None` means "unknown" (no server yet) and
/// shows nothing, so the rail never displays a stale or invented number.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NavCounters {
    /// Models in the library.
    pub models: Option<usize>,
    /// Active downloads.
    pub downloads: Option<usize>,
    /// Models loaded in memory.
    pub loaded: Option<usize>,
    /// Engine install jobs in flight.
    pub installs: Option<usize>,
}

/// How a rail counter is drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Counter {
    /// Plain mono number (Library).
    Plain(String),
    /// Number with a down arrow, in signal blue (Discover downloads).
    Downloads(String),
    /// Verdigris dot and number (Running).
    Live(String),
}

impl NavCounters {
    /// The counter for `screen`, if it has one to show. Zero downloads and zero loaded models
    /// show nothing (the design only shows activity); zero library models shows `0`.
    pub fn counter(&self, screen: Screen) -> Option<Counter> {
        match screen {
            Screen::Library => self.models.map(|n| Counter::Plain(n.to_string())),
            Screen::Discover => self
                .downloads
                .filter(|n| *n > 0)
                .map(|n| Counter::Downloads(format!("{n}\u{2193}"))),
            Screen::Running => self
                .loaded
                .filter(|n| *n > 0)
                .map(|n| Counter::Live(n.to_string())),
            Screen::Engines => self
                .installs
                .filter(|n| *n > 0)
                .map(|n| Counter::Downloads(format!("{n}\u{2193}"))),
            Screen::ApiKeys | Screen::Settings => None,
        }
    }
}

/// Status colour of the service card dot and title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Verdigris: running and healthy.
    Ok,
    /// Signal blue: starting, connecting.
    Info,
    /// Brass: needs the user (for example an API key).
    Attention,
    /// Cinnabar: failed.
    Danger,
    /// Lead: idle, not running.
    Muted,
}

/// What the service card at the bottom of the rail shows: state, address, service manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceCard {
    /// Dot and title colour.
    pub tone: Tone,
    /// First line (`Service running`).
    pub title: String,
    /// Server address, set in mono (`127.0.0.1:11435`).
    pub address: String,
    /// Last line (`systemd · alkahest.service`, an error, a hint).
    pub detail: String,
}

impl ServiceCard {
    /// The card before the first answer from the server.
    pub fn connecting(server_url: &str) -> Self {
        Self {
            tone: Tone::Info,
            title: "Connecting\u{2026}".into(),
            address: display_address(server_url),
            detail: "Alkahest server".into(),
        }
    }
}

/// `http://127.0.0.1:11435/` -> `127.0.0.1:11435`; anything that is not http(s) stays as is.
pub fn display_address(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))
        .unwrap_or(trimmed)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rail_lists_the_six_designed_screens_in_order() {
        let labels: Vec<_> = Screen::ALL.iter().map(|s| s.label()).collect();
        assert_eq!(
            labels,
            [
                "Library", "Discover", "Running", "Engines", "API keys", "Settings"
            ]
        );
    }

    #[test]
    fn unknown_counters_show_nothing() {
        let none = NavCounters::default();
        assert!(Screen::ALL.iter().all(|s| none.counter(*s).is_none()));
    }

    #[test]
    fn counters_follow_the_design() {
        let c = NavCounters {
            models: Some(14),
            downloads: Some(2),
            loaded: Some(4),
            installs: Some(1),
        };
        assert_eq!(
            c.counter(Screen::Library),
            Some(Counter::Plain("14".into()))
        );
        assert_eq!(
            c.counter(Screen::Discover),
            Some(Counter::Downloads("2\u{2193}".into()))
        );
        assert_eq!(c.counter(Screen::Running), Some(Counter::Live("4".into())));
        assert_eq!(
            c.counter(Screen::Engines),
            Some(Counter::Downloads("1\u{2193}".into()))
        );
        assert_eq!(c.counter(Screen::Settings), None);
        let idle = NavCounters {
            models: Some(0),
            downloads: Some(0),
            loaded: Some(0),
            installs: Some(0),
        };
        assert_eq!(
            idle.counter(Screen::Library),
            Some(Counter::Plain("0".into()))
        );
        assert_eq!(idle.counter(Screen::Running), None);
    }

    #[test]
    fn service_card_shows_the_bare_address() {
        assert_eq!(
            display_address("http://127.0.0.1:11435/"),
            "127.0.0.1:11435"
        );
        assert_eq!(display_address("https://gpu-box:443"), "gpu-box:443");
        let card = ServiceCard::connecting("http://127.0.0.1:11435");
        assert_eq!(card.tone, Tone::Info);
        assert_eq!(card.address, "127.0.0.1:11435");
    }
}
