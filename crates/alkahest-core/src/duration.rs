//! Ollama-compatible `keep_alive` durations.
//!
//! Ollama accepts Go duration strings (`"5m"`, `"1h30m"`, `"300ms"`), bare numbers meaning
//! seconds, any negative value meaning "keep loaded forever" and `0` meaning "unload now".

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::Error;

/// How long an idle model stays loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepAlive {
    /// Never unload (any negative input, canonical form `-1`).
    Forever,
    /// Unload after this idle time. `Duration::ZERO` unloads right after the request.
    For(Duration),
}

impl KeepAlive {
    /// Default idle time, matching Ollama: five minutes.
    pub const DEFAULT: KeepAlive = KeepAlive::For(Duration::from_secs(300));

    /// Returns the idle time, or `None` for [`KeepAlive::Forever`].
    pub fn as_duration(self) -> Option<Duration> {
        match self {
            Self::Forever => None,
            Self::For(d) => Some(d),
        }
    }

    /// Parses a Go-style duration (`"5m"`, `"1h30m"`, `"1.5h"`), a bare number of seconds
    /// (`"300"`, `"0.5"`), `"-1"` or any negative value (forever).
    pub fn parse(input: &str) -> Result<Self, Error> {
        let bad = || Error::InvalidDuration(input.to_string());
        let s = input.trim();
        let (negative, body) = match s.as_bytes().first() {
            None => return Err(bad()),
            Some(b'-') => (true, &s[1..]),
            Some(b'+') => (false, &s[1..]),
            Some(_) => (false, s),
        };
        if body.is_empty() {
            return Err(bad());
        }
        let nanos = parse_magnitude(body).ok_or_else(bad)?;
        if negative {
            // Any negative value means forever, as in Ollama; a signed zero is just zero.
            return Ok(if nanos == 0.0 {
                Self::For(Duration::ZERO)
            } else {
                Self::Forever
            });
        }
        if !nanos.is_finite() || nanos >= u64::MAX as f64 {
            return Err(bad());
        }
        Ok(Self::For(Duration::from_nanos(nanos as u64)))
    }
}

/// Parses an unsigned magnitude into nanoseconds. A bare number is seconds.
fn parse_magnitude(s: &str) -> Option<f64> {
    let is_num = |c: char| c.is_ascii_digit() || c == '.';
    if s.chars().all(is_num) {
        return s.parse::<f64>().ok().map(|secs| secs * 1e9);
    }
    let mut rest = s;
    let mut total = 0.0;
    while !rest.is_empty() {
        let num_end = rest.find(|c: char| !is_num(c)).unwrap_or(rest.len());
        if num_end == 0 {
            return None;
        }
        let value: f64 = rest[..num_end].parse().ok()?;
        rest = &rest[num_end..];
        let unit_end = rest.find(is_num).unwrap_or(rest.len());
        let scale = match &rest[..unit_end] {
            "ns" => 1.0,
            "us" | "µs" | "μs" => 1e3,
            "ms" => 1e6,
            "s" => 1e9,
            "m" => 60e9,
            "h" => 3600e9,
            _ => return None,
        };
        total += value * scale;
        rest = &rest[unit_end..];
    }
    Some(total)
}

impl FromStr for KeepAlive {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Error> {
        Self::parse(s)
    }
}

impl fmt::Display for KeepAlive {
    /// Canonical form: `-1`, `0`, or the largest exact unit (`1h`, `5m`, `90s`, `250ms`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let d = match self {
            Self::Forever => return f.write_str("-1"),
            Self::For(d) => *d,
        };
        if d.is_zero() {
            f.write_str("0")
        } else if d.subsec_nanos() % 1_000_000 != 0 {
            write!(f, "{}ns", d.as_nanos())
        } else if d.subsec_millis() != 0 {
            write!(f, "{}ms", d.as_millis())
        } else {
            let s = d.as_secs();
            if s % 3600 == 0 {
                write!(f, "{}h", s / 3600)
            } else if s % 60 == 0 {
                write!(f, "{}m", s / 60)
            } else {
                write!(f, "{s}s")
            }
        }
    }
}

impl Serialize for KeepAlive {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for KeepAlive {
    /// Accepts a string (`"5m"`) or an integer/float number of seconds.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Visitor;
        struct V;
        impl Visitor<'_> for V {
            type Value = KeepAlive;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a duration string such as \"5m\" or a number of seconds")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<KeepAlive, E> {
                KeepAlive::parse(v).map_err(E::custom)
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<KeepAlive, E> {
                Ok(match u64::try_from(v) {
                    Ok(s) => KeepAlive::For(Duration::from_secs(s)),
                    Err(_) => KeepAlive::Forever,
                })
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<KeepAlive, E> {
                Ok(KeepAlive::For(Duration::from_secs(v)))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<KeepAlive, E> {
                if v < 0.0 {
                    Ok(KeepAlive::Forever)
                } else if v.is_finite() && v < u64::MAX as f64 {
                    Ok(KeepAlive::For(Duration::from_secs_f64(v)))
                } else {
                    Err(E::custom("duration out of range"))
                }
            }
        }
        d.deserialize_any(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> KeepAlive {
        KeepAlive::For(Duration::from_secs(n))
    }

    #[test]
    fn parses_go_style_durations() {
        assert_eq!(KeepAlive::parse("5m").unwrap(), secs(300));
        assert_eq!(KeepAlive::parse("1h").unwrap(), secs(3600));
        assert_eq!(KeepAlive::parse("1h30m").unwrap(), secs(5400));
        assert_eq!(KeepAlive::parse("1.5h").unwrap(), secs(5400));
        assert_eq!(
            KeepAlive::parse("250ms").unwrap(),
            KeepAlive::For(Duration::from_millis(250))
        );
    }

    #[test]
    fn bare_numbers_are_seconds() {
        assert_eq!(KeepAlive::parse("300").unwrap(), secs(300));
        assert_eq!(
            KeepAlive::parse("0.5").unwrap(),
            KeepAlive::For(Duration::from_millis(500))
        );
    }

    #[test]
    fn zero_means_unload_now() {
        assert_eq!(KeepAlive::parse("0").unwrap(), secs(0));
        assert_eq!(KeepAlive::parse("0s").unwrap(), secs(0));
    }

    #[test]
    fn negative_means_forever() {
        for s in ["-1", "-1s", "-10m", "-0.5"] {
            assert_eq!(KeepAlive::parse(s).unwrap(), KeepAlive::Forever, "{s}");
        }
        assert_eq!(KeepAlive::Forever.as_duration(), None);
    }

    #[test]
    fn rejects_garbage() {
        for s in ["", "-", "abc", "5x", "m5", "1h-5m", "inf", "nan", "5 m"] {
            assert!(KeepAlive::parse(s).is_err(), "{s:?} should fail");
        }
    }

    #[test]
    fn display_round_trips() {
        for ka in [
            KeepAlive::Forever,
            secs(0),
            secs(300),
            secs(3600),
            secs(90),
            KeepAlive::For(Duration::from_millis(250)),
        ] {
            assert_eq!(KeepAlive::parse(&ka.to_string()).unwrap(), ka);
        }
        assert_eq!(secs(300).to_string(), "5m");
    }

    #[test]
    fn serde_accepts_string_and_integer() {
        #[derive(Deserialize)]
        struct T {
            k: KeepAlive,
        }
        assert_eq!(toml::from_str::<T>("k = \"10m\"").unwrap().k, secs(600));
        assert_eq!(toml::from_str::<T>("k = 120").unwrap().k, secs(120));
        assert_eq!(toml::from_str::<T>("k = -1").unwrap().k, KeepAlive::Forever);
    }
}
