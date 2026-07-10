//! Date-encoding detection + normalization.
//! All discovered encodings are normalized AT INDEX TIME to RFC3339 UTC with
//! millis; mappings always say `strict_date_optional_time||epoch_millis`.
//! (The engine's dynamic mapping has NO date detection — verified — so
//! autoindex is the date layer.)

use chrono::{DateTime, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone, Utc};

/// Epoch windows: 1990-01-01 .. 2100-01-01
pub const EPOCH_MS_MIN: i64 = 631_152_000_000;
pub const EPOCH_MS_MAX: i64 = 4_102_444_800_000;
pub const EPOCH_S_MIN: i64 = 631_152_000;
pub const EPOCH_S_MAX: i64 = 4_102_444_800;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DateEnc {
    Rfc3339,
    IsoNaive,     // 2026-03-17T00:00:13(.529)? — assumed UTC
    SpaceNaive,   // 2026-03-17 00:00:13(,529)? — assumed UTC
    DateOnly,     // 2026-03-17
    Clf,          // 01/Mar/2026:00:00:03 +0000
    Rfc2822,      // Tue, 01 Jul 2025 10:00:00 +0000
    EpochMillis,  // numeric
    EpochSeconds, // numeric
}

impl DateEnc {
    pub fn as_str(&self) -> &'static str {
        match self {
            DateEnc::Rfc3339 => "rfc3339",
            DateEnc::IsoNaive => "iso-naive (assumed UTC)",
            DateEnc::SpaceNaive => "yyyy-mm-dd hh:mm:ss (assumed UTC)",
            DateEnc::DateOnly => "date-only",
            DateEnc::Clf => "CLF (dd/Mon/yyyy:HH:MM:SS zz)",
            DateEnc::Rfc2822 => "rfc2822",
            DateEnc::EpochMillis => "epoch-millis",
            DateEnc::EpochSeconds => "epoch-seconds",
        }
    }
}

/// Try all string encodings in priority order.
pub fn parse_date_str(s: &str) -> Option<(DateTime<Utc>, DateEnc)> {
    let t = s.trim();
    if t.len() < 8 || t.len() > 40 {
        return None;
    }
    // cheap prefilter: must start with a digit or weekday/uppercase letter
    let c0 = t.as_bytes()[0];
    if !c0.is_ascii_digit() && !c0.is_ascii_uppercase() {
        return None;
    }

    if c0.is_ascii_digit() {
        // 1. RFC3339 / ISO with zone
        if let Ok(dt) = DateTime::parse_from_rfc3339(t) {
            return Some((dt.with_timezone(&Utc), DateEnc::Rfc3339));
        }
        // 2. ISO naive with T
        if t.len() >= 19 && t.as_bytes()[10] == b'T' {
            let norm = t.replace(',', ".");
            if let Ok(ndt) = NaiveDateTime::parse_from_str(&norm, "%Y-%m-%dT%H:%M:%S%.f") {
                return Some((Utc.from_utc_datetime(&ndt), DateEnc::IsoNaive));
            }
        }
        // 3. space-separated (java/log4j/python logging), comma millis
        if t.len() >= 19 && t.as_bytes()[10] == b' ' {
            let norm = t.replace(',', ".");
            if let Ok(ndt) = NaiveDateTime::parse_from_str(&norm, "%Y-%m-%d %H:%M:%S%.f") {
                return Some((Utc.from_utc_datetime(&ndt), DateEnc::SpaceNaive));
            }
        }
        // 4. date only
        if t.len() == 10 {
            if let Ok(nd) = NaiveDate::parse_from_str(t, "%Y-%m-%d") {
                let ndt = nd.and_hms_opt(0, 0, 0)?;
                return Some((Utc.from_utc_datetime(&ndt), DateEnc::DateOnly));
            }
        }
        // 5. CLF
        if t.len() >= 20 && t.as_bytes()[2] == b'/' {
            if let Ok(dt) = DateTime::parse_from_str(t, "%d/%b/%Y:%H:%M:%S %z") {
                return Some((dt.with_timezone(&Utc), DateEnc::Clf));
            }
        }
    } else {
        // 6. RFC2822 (Tue, 01 Jul 2025 10:00:00 +0000 / GMT)
        if let Ok(dt) = DateTime::parse_from_rfc2822(t) {
            return Some((dt.with_timezone(&Utc), DateEnc::Rfc2822));
        }
    }
    None
}

/// Interpret an integer as epoch millis/seconds if within the window.
pub fn parse_epoch(n: i64) -> Option<(DateTime<Utc>, DateEnc)> {
    if (EPOCH_MS_MIN..=EPOCH_MS_MAX).contains(&n) {
        return Utc
            .timestamp_millis_opt(n)
            .single()
            .map(|dt| (dt, DateEnc::EpochMillis));
    }
    if (EPOCH_S_MIN..=EPOCH_S_MAX).contains(&n) {
        return Utc
            .timestamp_opt(n, 0)
            .single()
            .map(|dt| (dt, DateEnc::EpochSeconds));
    }
    None
}

pub fn to_rfc3339_millis(dt: &DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Coerce any raw value to the normalized date string given the elected
/// encoding (falls back to trying everything).
pub fn coerce_to_date(v: &serde_json::Value, elected: Option<DateEnc>) -> Option<String> {
    match v {
        serde_json::Value::String(s) => {
            if let Some((dt, _)) = parse_date_str(s) {
                return Some(to_rfc3339_millis(&dt));
            }
            // numeric string epoch?
            if let Ok(n) = s.trim().parse::<i64>() {
                if let Some((dt, _)) = parse_epoch(n) {
                    return Some(to_rfc3339_millis(&dt));
                }
            }
            None
        }
        serde_json::Value::Number(n) => {
            let i = n.as_i64().or_else(|| n.as_f64().map(|f| f as i64))?;
            match elected {
                Some(DateEnc::EpochSeconds) => {
                    if (EPOCH_S_MIN..=EPOCH_S_MAX).contains(&i) {
                        Utc.timestamp_opt(i, 0)
                            .single()
                            .map(|d| to_rfc3339_millis(&d))
                    } else {
                        parse_epoch(i).map(|(d, _)| to_rfc3339_millis(&d))
                    }
                }
                _ => parse_epoch(i).map(|(d, _)| to_rfc3339_millis(&d)),
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_encodings() {
        for (s, enc) in [
            ("2026-03-04T16:00:00.319Z", DateEnc::Rfc3339),
            ("2026-03-04T16:00:00+02:00", DateEnc::Rfc3339),
            ("2026-03-17T00:00:13.529", DateEnc::IsoNaive),
            ("2026-03-17 00:00:13,529", DateEnc::SpaceNaive),
            ("2026-03-03 15:11:18", DateEnc::SpaceNaive),
            ("2026-03-19", DateEnc::DateOnly),
            ("01/Mar/2026:00:00:03 +0000", DateEnc::Clf),
            ("Tue, 01 Jul 2025 10:00:00 +0000", DateEnc::Rfc2822),
            ("Tue, 01 Jul 2025 10:00:00 GMT", DateEnc::Rfc2822),
        ] {
            let (dt, e) = parse_date_str(s).unwrap_or_else(|| panic!("failed: {s}"));
            assert_eq!(e, enc, "{s}");
            assert!(dt.timestamp() > 0);
        }
        assert_eq!(parse_epoch(1773107846071).unwrap().1, DateEnc::EpochMillis);
        assert_eq!(parse_epoch(1773107846).unwrap().1, DateEnc::EpochSeconds);
        assert!(parse_epoch(42).is_none());
        assert!(parse_date_str("not a date").is_none());
        assert!(parse_date_str("1.2.3").is_none());
    }

    #[test]
    fn normalization() {
        let (dt, _) = parse_date_str("01/Mar/2026:00:00:03 +0000").unwrap();
        assert_eq!(to_rfc3339_millis(&dt), "2026-03-01T00:00:03.000Z");
    }
}
