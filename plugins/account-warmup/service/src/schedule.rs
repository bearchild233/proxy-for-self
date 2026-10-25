use std::collections::HashSet;

use chrono::{DateTime, Days, LocalResult, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use plugin_sdk::{
    RpcResult,
    protocol::{INVALID_PARAMS, RpcError},
};

use crate::model::Settings;

pub fn invalid(message: &str) -> RpcError {
    RpcError::new(INVALID_PARAMS, message)
}

pub fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

pub fn ids(values: &[String]) -> RpcResult<()> {
    let mut seen = HashSet::new();
    if values.len() > 500
        || values
            .iter()
            .any(|value| !identifier(value) || !seen.insert(value))
    {
        return Err(invalid("accountIds must contain at most 500 unique IDs"));
    }
    Ok(())
}

pub fn time(value: &str) -> Option<NaiveTime> {
    if value.len() != 5 || value.as_bytes().get(2) != Some(&b':') {
        return None;
    }
    NaiveTime::parse_from_str(value, "%H:%M").ok()
}

pub fn validate(settings: &Settings) -> RpcResult<()> {
    ids(&settings.account_ids)?;
    let mut overrides = HashSet::new();
    if settings.timezone.parse::<Tz>().is_err()
        || time(&settings.daily_time).is_none()
        || !matches!(settings.target_mode.as_str(), "all" | "selected")
        || !identifier(&settings.default_model)
        || settings.jitter_min_seconds > settings.jitter_max_seconds
        || settings.jitter_max_seconds > 300
        || settings.reset_delay_seconds > 86400
        || (settings.target_mode == "selected" && settings.account_ids.is_empty())
        || (settings.enabled && !settings.daily_enabled && !settings.reset_enabled)
        || settings.account_overrides.len() > 500
    {
        return Err(invalid("invalid warmup settings"));
    }
    for item in &settings.account_overrides {
        if !identifier(&item.account_id)
            || !overrides.insert(&item.account_id)
            || item
                .daily_time
                .as_deref()
                .is_some_and(|value| time(value).is_none())
            || item
                .model
                .as_deref()
                .is_some_and(|value| !identifier(value))
            || item.reset_delay_seconds.is_some_and(|value| value > 86400)
        {
            return Err(invalid("invalid account override"));
        }
    }
    Ok(())
}

/// DST 重叠选择较早映射；缺失时刻推进到当日下一个有效分钟。
fn local_at(date: chrono::NaiveDate, at: NaiveTime, tz: Tz) -> Option<DateTime<Utc>> {
    let naive = date.and_time(at);
    for offset in 0..=180 {
        let candidate = naive + chrono::Duration::minutes(offset);
        if candidate.date() != date {
            break;
        }
        match tz.from_local_datetime(&candidate) {
            LocalResult::Single(value) | LocalResult::Ambiguous(value, _) => {
                return Some(value.with_timezone(&Utc));
            }
            LocalResult::None => {}
        }
    }
    None
}

pub fn daily_points(
    now: DateTime<Utc>,
    timezone: &str,
    daily_time: &str,
) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let tz = timezone.parse::<Tz>().ok()?;
    let at = time(daily_time)?;
    let today = now.with_timezone(&tz).date_naive();
    let current = local_at(today, at, tz)?;
    if current <= now {
        Some((
            current,
            local_at(today.checked_add_days(Days::new(1))?, at, tz)?,
        ))
    } else {
        Some((
            local_at(today.checked_sub_days(Days::new(1))?, at, tz)?,
            current,
        ))
    }
}

/// FNV-1a 固定字节算法；同一事件跨重启/版本产生同一个有界抖动。
pub fn jitter(key: &str, minimum: u32, maximum: u32) -> u32 {
    let hash = key.bytes().fold(14695981039346656037_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(1099511628211)
    });
    minimum + u32::try_from(hash % u64::from(maximum - minimum + 1)).expect("bounded jitter")
}
