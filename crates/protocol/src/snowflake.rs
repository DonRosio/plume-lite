use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Plume Lite epoch (2025-01-01T00:00:00Z) in Unix milliseconds.
pub const EPOCH_MS: i64 = 1_735_689_600_000;

/// Bits reserved for the worker id and the per-millisecond sequence.
pub const TIMESTAMP_SHIFT: u32 = 22;

/// A 64-bit, roughly time-ordered unique identifier (Twitter/Discord style).
///
/// Layout (most significant first): 41 bits of milliseconds since [`EPOCH_MS`],
/// 10 bits of worker id, 12 bits of sequence. Sorting by id sorts by creation
/// time, which gives us cheap cursor pagination and a free `created_at`.
///
/// Serialised as a JSON string so that JavaScript clients never lose precision.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[cfg_attr(feature = "sqlx", derive(sqlx::Type), sqlx(transparent))]
pub struct Snowflake(pub i64);

impl Snowflake {
    /// Milliseconds since the Unix epoch at which this id was generated.
    pub fn timestamp_ms(self) -> i64 {
        (self.0 >> TIMESTAMP_SHIFT) + EPOCH_MS
    }

    /// Creation time encoded in the id.
    pub fn created_at(self) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(self.timestamp_ms()).unwrap_or_default()
    }

    /// Smallest id that could have been generated at `unix_ms`. Handy for time-based cursors.
    pub fn from_timestamp_ms(unix_ms: i64) -> Self {
        Self((unix_ms - EPOCH_MS).max(0) << TIMESTAMP_SHIFT)
    }
}

impl fmt::Display for Snowflake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl fmt::Debug for Snowflake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Snowflake({})", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseSnowflakeError;

impl fmt::Display for ParseSnowflakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid snowflake id")
    }
}

impl std::error::Error for ParseSnowflakeError {}

impl FromStr for Snowflake {
    type Err = ParseSnowflakeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.parse::<i64>() {
            Ok(v) if v >= 0 => Ok(Self(v)),
            _ => Err(ParseSnowflakeError),
        }
    }
}

impl Serialize for Snowflake {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Snowflake {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SnowflakeVisitor;

        impl Visitor<'_> for SnowflakeVisitor {
            type Value = Snowflake;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a snowflake id as a string or a non-negative integer")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Snowflake, E> {
                v.parse().map_err(E::custom)
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Snowflake, E> {
                if v < 0 {
                    return Err(E::custom(ParseSnowflakeError));
                }
                Ok(Snowflake(v))
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Snowflake, E> {
                i64::try_from(v).map(Snowflake).map_err(|_| E::custom(ParseSnowflakeError))
            }
        }

        deserializer.deserialize_any(SnowflakeVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialises_as_string_and_accepts_both_forms() {
        let id = Snowflake(1_234_567_890_123);
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"1234567890123\"");
        assert_eq!(serde_json::from_str::<Snowflake>("\"42\"").unwrap(), Snowflake(42));
        assert_eq!(serde_json::from_str::<Snowflake>("42").unwrap(), Snowflake(42));
        assert!(serde_json::from_str::<Snowflake>("\"-1\"").is_err());
        assert!(serde_json::from_str::<Snowflake>("\"abc\"").is_err());
    }

    #[test]
    fn timestamp_roundtrip() {
        let ms = EPOCH_MS + 86_400_000;
        let id = Snowflake::from_timestamp_ms(ms);
        assert_eq!(id.timestamp_ms(), ms);
        assert_eq!(id.created_at().timestamp_millis(), ms);
    }
}
