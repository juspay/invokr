use crate::models::pg_cron_expr::PgCronExpr;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

/// Lenient deserialization for the timestamp fields on job creation/update.
///
/// HTML `<input type="datetime-local">` (used by the dashboard's job form)
/// emits values like `2026-06-09T17:18` — no seconds and no timezone offset.
/// chrono's default `DateTime<Utc>` deserializer only accepts full RFC 3339
/// (`2026-06-09T17:18:00Z`), so those values failed with the opaque
/// "premature end of input" serde error. This accepts both, treating
/// timezone-less values as UTC.
mod flexible_datetime {
    use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
    use serde::{Deserialize, Deserializer};

    pub fn deserialize_opt<'de, D>(deserializer: D) -> Result<Option<DateTime<Utc>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        match Option::<String>::deserialize(deserializer)? {
            None => Ok(None),
            Some(s) if s.trim().is_empty() => Ok(None),
            Some(s) => parse(s.trim()).map(Some).map_err(serde::de::Error::custom),
        }
    }

    fn parse(s: &str) -> Result<DateTime<Utc>, String> {
        // Absolute instant carrying an offset or `Z`.
        if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
            return Ok(dt.with_timezone(&Utc));
        }
        // `datetime-local` wall-clock value, with or without seconds.
        // Interpreted as UTC.
        for fmt in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M"] {
            if let Ok(naive) = NaiveDateTime::parse_from_str(s, fmt) {
                return Ok(Utc.from_utc_datetime(&naive));
            }
        }
        Err(format!(
            "invalid datetime '{s}': expected an RFC 3339 timestamp \
             (e.g. 2026-06-09T17:18:00Z) or a datetime-local value \
             (e.g. 2026-06-09T17:18)"
        ))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn accepts_datetime_local_without_seconds() {
            assert_eq!(
                parse("2026-06-09T17:18").unwrap(),
                Utc.with_ymd_and_hms(2026, 6, 9, 17, 18, 0).unwrap()
            );
        }

        #[test]
        fn accepts_datetime_local_with_seconds() {
            assert_eq!(
                parse("2026-06-09T17:18:30").unwrap(),
                Utc.with_ymd_and_hms(2026, 6, 9, 17, 18, 30).unwrap()
            );
        }

        #[test]
        fn accepts_rfc3339_with_offset() {
            assert_eq!(
                parse("2026-06-09T17:18:00+05:30").unwrap(),
                Utc.with_ymd_and_hms(2026, 6, 9, 11, 48, 0).unwrap()
            );
        }

        #[test]
        fn rejects_garbage() {
            assert!(parse("not a date").is_err());
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
// TODO 1: Use strum macros and serde to automatically make enums from strings
pub enum TriggerType {
    IMMEDIATE,
    DELAYED,
    CRON,
}

impl TriggerType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::IMMEDIATE => "IMMEDIATE",
            Self::DELAYED => "DELAYED",
            Self::CRON => "CRON",
        }
    }

    pub fn from_str_val(s: &str) -> Option<Self> {
        match s {
            "IMMEDIATE" => Some(Self::IMMEDIATE),
            "DELAYED" => Some(Self::DELAYED),
            "CRON" => Some(Self::CRON),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum JobStatus {
    ACTIVE,
    RETIRED,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ACTIVE => "ACTIVE",
            Self::RETIRED => "RETIRED",
        }
    }

    pub fn from_str_val(s: &str) -> Option<Self> {
        match s {
            "ACTIVE" => Some(Self::ACTIVE),
            "RETIRED" => Some(Self::RETIRED),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Job {
    pub job_id: String,
    pub endpoint: String,
    pub endpoint_type: String,
    pub trigger_type: String,
    pub status: String,
    pub version: i64,
    pub previous_version_id: Option<String>,
    pub replaced_by_id: Option<String>,
    pub idempotency_key: Option<String>,
    pub input: Option<serde_json::Value>,
    pub run_at: Option<DateTime<Utc>>,
    pub cron_expression: Option<String>,
    pub cron_timezone: Option<String>,
    pub cron_starts_at: Option<DateTime<Utc>>,
    pub cron_ends_at: Option<DateTime<Utc>>,
    pub cron_next_run_at: Option<DateTime<Utc>>,
    pub cron_last_tick_at: Option<DateTime<Utc>>,
    pub async_max_wait_ms: Option<i64>,
    pub async_max_polls: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub retired_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AsyncOverrides {
    pub max_wait_ms: Option<i64>,
    pub max_polls: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct CreateJob {
    pub endpoint: String,
    pub trigger: String,
    pub idempotency_key: Option<String>,
    pub input: Option<serde_json::Value>,
    #[serde(default)]
    pub max_attempts: Option<i64>,
    #[serde(default, deserialize_with = "flexible_datetime::deserialize_opt")]
    pub run_at: Option<DateTime<Utc>>,
    pub cron: Option<PgCronExpr>,
    pub timezone: Option<String>,
    #[serde(default, deserialize_with = "flexible_datetime::deserialize_opt")]
    pub starts_at: Option<DateTime<Utc>>,
    #[serde(default, deserialize_with = "flexible_datetime::deserialize_opt")]
    pub ends_at: Option<DateTime<Utc>>,
    pub async_overrides: Option<AsyncOverrides>,
}

pub fn resolve_async_bounds(
    overrides: Option<&AsyncOverrides>,
    endpoint_async: Option<(i64, i32)>,
) -> Result<Option<(i64, i32)>, String> {
    if overrides.is_some() && endpoint_async.is_none() {
        return Err("async_overrides given but endpoint has no async block".into());
    }
    let Some((ep_wait, ep_polls)) = endpoint_async else {
        return Ok(None);
    };

    let Some(o) = overrides else {
        // No overrides — trust endpoint defaults (already validated at endpoint create time).
        return Ok(Some((ep_wait, ep_polls)));
    };

    let wait = o.max_wait_ms.unwrap_or(ep_wait);
    let polls = o.max_polls.unwrap_or(ep_polls);

    if !(1..=30 * 24 * 3600 * 1000).contains(&wait) {
        return Err("async_overrides.max_wait_ms out of range (1 .. 30d)".into());
    }
    if !(1..=100_000).contains(&polls) {
        return Err("async_overrides.max_polls out of range (1 .. 100000)".into());
    }
    Ok(Some((wait, polls)))
}

#[cfg(test)]
mod async_overrides_tests {
    use super::*;

    pub fn resolve(
        overrides: Option<&AsyncOverrides>,
        endpoint_async: Option<(i64, i32)>,
    ) -> Result<Option<(i64, i32)>, String> {
        crate::models::job::resolve_async_bounds(overrides, endpoint_async)
    }

    #[test]
    fn rejects_overrides_when_endpoint_not_async() {
        let o = AsyncOverrides {
            max_wait_ms: Some(60_000),
            max_polls: None,
        };
        assert!(resolve(Some(&o), None).is_err());
    }

    #[test]
    fn falls_back_to_endpoint_defaults() {
        let o = AsyncOverrides {
            max_wait_ms: None,
            max_polls: None,
        };
        let got = resolve(Some(&o), Some((60_000, 100))).unwrap();
        assert_eq!(got, Some((60_000, 100)));
    }

    #[test]
    fn applies_partial_override() {
        let o = AsyncOverrides {
            max_wait_ms: Some(120_000),
            max_polls: None,
        };
        let got = resolve(Some(&o), Some((60_000, 100))).unwrap();
        assert_eq!(got, Some((120_000, 100)));
    }

    #[test]
    fn out_of_range_override_rejected() {
        let o = AsyncOverrides {
            max_wait_ms: Some(0),
            max_polls: None,
        };
        assert!(resolve(Some(&o), Some((60_000, 100))).is_err());
    }

    #[test]
    fn returns_endpoint_defaults_without_overrides() {
        assert_eq!(
            resolve(None, Some((60_000, 100))).unwrap(),
            Some((60_000, 100))
        );
    }

    #[test]
    fn returns_none_when_endpoint_not_async_and_no_overrides() {
        assert_eq!(resolve(None, None).unwrap(), None);
    }

    #[test]
    fn out_of_range_polls_rejected() {
        let o = AsyncOverrides {
            max_wait_ms: None,
            max_polls: Some(100_001),
        };
        assert!(resolve(Some(&o), Some((60_000, 100))).is_err());
    }
}

#[derive(Debug, Deserialize)]
pub struct UpdateJob {
    pub cron: Option<PgCronExpr>,
    pub timezone: Option<String>,
    pub input: Option<serde_json::Value>,
    #[serde(default, deserialize_with = "flexible_datetime::deserialize_opt")]
    pub starts_at: Option<DateTime<Utc>>,
    #[serde(default, deserialize_with = "flexible_datetime::deserialize_opt")]
    pub ends_at: Option<DateTime<Utc>>,
}
