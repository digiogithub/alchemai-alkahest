//! Download settings (`[pull]` in the configuration file).

use serde::{Deserialize, Serialize};

/// Model download settings.
///
/// ```toml
/// [pull]
/// max_bytes_per_sec = 5_000_000   # cap `alkahest pull` at about 5 MB/s
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PullConfig {
    /// Bandwidth cap of one `pull` in bytes per second, shared by all its connections.
    /// Absent or `0` means unlimited. Overridden by `ALKAHEST_PULL_MAX_BPS`.
    pub max_bytes_per_sec: Option<u64>,
}

impl PullConfig {
    /// The effective cap: `None` for unlimited (absent or zero).
    pub fn bandwidth_cap(&self) -> Option<u64> {
        self.max_bytes_per_sec.filter(|&n| n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_and_absent_mean_unlimited() {
        assert_eq!(PullConfig::default().bandwidth_cap(), None);
        assert_eq!(
            PullConfig {
                max_bytes_per_sec: Some(0)
            }
            .bandwidth_cap(),
            None
        );
        assert_eq!(
            PullConfig {
                max_bytes_per_sec: Some(1000)
            }
            .bandwidth_cap(),
            Some(1000)
        );
    }
}
