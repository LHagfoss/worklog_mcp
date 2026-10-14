//! Europe/Oslo time helpers. The user works in Oslo local time, but the
//! Solidtime API requires UTC ISO-8601 (`Y-m-dTH:i:sZ`). Every conversion in
//! this crate goes through here so an offset is never hand-computed.

use anyhow::{Context, Result, bail};
use chrono::{
    DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc,
};
use chrono_tz::Tz;

/// The user's timezone.
pub const OSLO: Tz = chrono_tz::Europe::Oslo;

/// Parse a `YYYY-MM-DD` date.
pub fn parse_date(value: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d")
        .with_context(|| format!("invalid date '{value}', expected YYYY-MM-DD"))
}

/// Parse an `HH:MM` (or `HH:MM:SS`) local time.
pub fn parse_time(value: &str) -> Result<NaiveTime> {
    let trimmed = value.trim();
    NaiveTime::parse_from_str(trimmed, "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(trimmed, "%H:%M:%S"))
        .with_context(|| format!("invalid time '{value}', expected HH:MM"))
}

fn resolve_local(naive: NaiveDateTime) -> Result<DateTime<Tz>> {
    match OSLO.from_local_datetime(&naive) {
        LocalResult::Single(dt) => Ok(dt),
        LocalResult::Ambiguous(first, _) => Ok(first),
        LocalResult::None => {
            bail!("local time {naive} does not exist in Europe/Oslo (daylight-saving gap)")
        }
    }
}

/// Convert an Oslo-local date + time into UTC.
pub fn local_to_utc(date: &str, time: &str) -> Result<DateTime<Utc>> {
    let naive = parse_date(date)?.and_time(parse_time(time)?);
    Ok(resolve_local(naive)?.with_timezone(&Utc))
}

/// Start of an Oslo-local day, as UTC (inclusive).
pub fn day_start_utc(date: NaiveDate) -> DateTime<Utc> {
    let naive = date.and_hms_opt(0, 0, 0).expect("midnight is valid");
    resolve_local(naive)
        .expect("midnight always exists in Oslo")
        .with_timezone(&Utc)
}

/// Exclusive end of an Oslo-local day, as UTC.
pub fn day_end_utc(date: NaiveDate) -> DateTime<Utc> {
    day_start_utc(date + Duration::days(1))
}

/// Monday..Sunday range containing `date`, as Oslo-local dates.
pub fn week_bounds(date: NaiveDate) -> (NaiveDate, NaiveDate) {
    let monday = date - Duration::days(date.weekday().num_days_from_monday() as i64);
    (monday, monday + Duration::days(6))
}

/// Today in Oslo local time.
pub fn today_local() -> NaiveDate {
    Utc::now().with_timezone(&OSLO).date_naive()
}

/// UTC DateTime -> `Y-m-dTH:i:sZ`.
pub fn fmt_utc(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// UTC DateTime -> Oslo-local `HH:MM`.
pub fn fmt_local_hm(dt: DateTime<Utc>) -> String {
    dt.with_timezone(&OSLO).format("%H:%M").to_string()
}

/// Parse a UTC ISO-8601 timestamp as returned by the API.
pub fn parse_utc(value: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .or_else(|_| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%SZ").map(|naive| naive.and_utc())
        })
        .with_context(|| format!("invalid timestamp '{value}'"))
}

/// Duration in seconds -> hours, rounded to two decimals.
pub fn hours(seconds: i64) -> f64 {
    (seconds as f64 / 3600.0 * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summer_offset_is_two_hours() {
        let utc = local_to_utc("2026-08-17", "08:00").unwrap();
        assert_eq!(fmt_utc(utc), "2026-08-17T06:00:00Z");
    }

    #[test]
    fn winter_offset_is_one_hour() {
        let utc = local_to_utc("2026-01-15", "08:00").unwrap();
        assert_eq!(fmt_utc(utc), "2026-01-15T07:00:00Z");
    }

    #[test]
    fn week_bounds_start_monday() {
        let (mon, sun) = week_bounds(parse_date("2026-10-09").unwrap());
        assert_eq!(mon.to_string(), "2026-10-05");
        assert_eq!(sun.to_string(), "2026-10-11");
    }
}
