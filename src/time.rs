//! Timestamps are stored as fixed-width RFC 3339 UTC text, so they sort and
//! compare correctly as plain strings in SQL.

use chrono::{DateTime, SecondsFormat, Utc};

pub fn ts(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn now_ts() -> String {
    ts(Utc::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn timestamps_are_fixed_width_utc() {
        let t = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();
        assert_eq!(ts(t), "2026-01-02T03:04:05.000Z");
        assert!(ts(t) < ts(t + chrono::Duration::milliseconds(1)));
    }
}
