//! Date-encoding detection + normalization.
//! All discovered encodings are normalized AT INDEX TIME to RFC3339 UTC with
//! millis; mappings always say `strict_date_optional_time||epoch_millis`.
//! (The engine's dynamic mapping has NO date detection — verified — so
//! autoindex is the date layer.)

use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, SecondsFormat, TimeZone, Utc, Weekday};

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
    /// vsftpd/wu-ftpd `xferlog` current-time: `Fri Oct 16 09:06:58 2026`
    /// (ctime: weekday, month, day, time, year). No zone, so UTC is assumed,
    /// the same assumption as the other naive encodings. Declared ahead of
    /// [`DateEnc::BsdSyslog`] because it carries a year.
    Asctime,
    /// RFC 3164 timestamp: `Oct 17 22:26:01` / `Oct  5 09:06:58`. No year and
    /// no zone. Appended last so a tie still prefers every encoding above,
    /// which all carry a year.
    BsdSyslog,
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
            DateEnc::Asctime => "asctime (Www Mmm dd hh:mm:ss yyyy, assumed UTC)",
            // The year is not in the stamp. It is the year of `now` unless
            // that reading is more than 24h in the future, in which case it
            // is the previous year (a December line read in January).
            DateEnc::BsdSyslog => {
                "bsd-syslog (year assumed: current UTC year, or the previous year when the stamp is more than 24h ahead)"
            }
        }
    }
}

/// Try all string encodings in priority order. A year-less BSD syslog
/// stamp takes its year from [`Utc::now`] — see [`DateEnc::BsdSyslog`].
pub fn parse_date_str(s: &str) -> Option<(DateTime<Utc>, DateEnc)> {
    parse_date_str_at(s, Utc::now())
}

/// [`parse_date_str`] with the clock the BSD-syslog year rule reads.
/// Every other encoding ignores `now`.
pub fn parse_date_str_at(s: &str, now: DateTime<Utc>) -> Option<(DateTime<Utc>, DateEnc)> {
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
        if let Some(dt) = parse_asctime(t) {
            return Some((dt, DateEnc::Asctime));
        }
        if let Some(dt) = parse_bsd_syslog(t, now) {
            return Some((dt, DateEnc::BsdSyslog));
        }
    }
    None
}

/// English month abbreviations. Chrono's `%b` follows the process locale, so
/// a stamp of `Oct` must not depend on `LC_TIME`.
fn english_month(name: &str) -> Option<u32> {
    Some(match name {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    })
}

/// `Www Mmm dd hh:mm:ss yyyy`, the whole string and nothing else. The weekday
/// has to be the weekday of that civil date: a mismatched name is not a
/// transfer-log stamp. No zone is present, so the instant is UTC.
fn parse_asctime(t: &str) -> Option<DateTime<Utc>> {
    let mut parts = t.split_whitespace();
    let weekday = english_weekday(parts.next()?)?;
    let month = english_month(parts.next()?)?;
    let day: u32 = parts.next()?.parse().ok()?;
    let time = parts.next()?;
    let year: i32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=31).contains(&day) || time.len() != 8 {
        return None;
    }
    let (hour, minute, second) = hms(time)?;
    let date = NaiveDate::from_ymd_opt(year, month, day)?;
    if date.weekday() != weekday {
        return None;
    }
    date.and_hms_opt(hour, minute, second)
        .map(|ndt| Utc.from_utc_datetime(&ndt))
}

fn english_weekday(name: &str) -> Option<Weekday> {
    Some(match name {
        "Mon" => Weekday::Mon,
        "Tue" => Weekday::Tue,
        "Wed" => Weekday::Wed,
        "Thu" => Weekday::Thu,
        "Fri" => Weekday::Fri,
        "Sat" => Weekday::Sat,
        "Sun" => Weekday::Sun,
        _ => return None,
    })
}

fn hms(time: &str) -> Option<(u32, u32, u32)> {
    let mut hm = time.split(':');
    let hour: u32 = hm.next()?.parse().ok()?;
    let minute: u32 = hm.next()?.parse().ok()?;
    let second: u32 = hm.next()?.parse().ok()?;
    if hm.next().is_some() || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some((hour, minute, second))
}

/// `Mmm dd hh:mm:ss` (day space-padded), the whole string and nothing else.
/// Year: `now`'s UTC year, unless that instant is more than 24 hours ahead
/// of `now`, in which case the previous year. A Feb 29 that is not a real
/// day in the chosen year does not parse — inventing a different day would
/// put the line on the wrong date.
fn parse_bsd_syslog(t: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let mut parts = t.split_whitespace();
    let month = english_month(parts.next()?)?;
    let day: u32 = parts.next()?.parse().ok()?;
    let time = parts.next()?;
    if parts.next().is_some() || !(1..=31).contains(&day) || time.len() != 8 {
        return None;
    }
    let (hour, minute, second) = hms(time)?;
    let at = |year: i32| {
        NaiveDate::from_ymd_opt(year, month, day)?
            .and_hms_opt(hour, minute, second)
            .map(|ndt| Utc.from_utc_datetime(&ndt))
    };
    let year = now.year();
    let dt = at(year)?;
    match now.checked_add_signed(chrono::Duration::hours(24)) {
        Some(limit) if dt > limit => at(year - 1),
        _ => Some(dt),
    }
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

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    /// RFC 3164 has no year. Leaving the stamp as keyword makes a time-range
    /// filter miss every line. The year is the clock's year unless that
    /// reading is more than a day ahead (a December line indexed in January).
    #[test]
    fn bsd_syslog_stamps_are_dates_with_an_assumed_year() {
        let now = at(2026, 10, 17, 23, 0, 0);
        let (dt, enc) = parse_date_str_at("Oct 17 22:26:01", now).unwrap();
        assert_eq!(enc, DateEnc::BsdSyslog);
        assert_eq!(to_rfc3339_millis(&dt), "2026-10-17T22:26:01.000Z");

        let (padded, _) = parse_date_str_at("Oct  5 09:06:58", now).unwrap();
        assert_eq!(to_rfc3339_millis(&padded), "2026-10-05T09:06:58.000Z");

        // Read in January: December is more than 24h ahead if taken as
        // this year, so the stamp is last year.
        let jan = at(2026, 1, 2, 0, 0, 0);
        let (rolled, _) = parse_date_str_at("Dec 31 23:59:59", jan).unwrap();
        assert_eq!(to_rfc3339_millis(&rolled), "2025-12-31T23:59:59.000Z");

        // A few minutes of clock skew stays in the current year.
        let early = at(2026, 10, 17, 22, 0, 0);
        let (skew, _) = parse_date_str_at("Oct 17 22:26:01", early).unwrap();
        assert_eq!(to_rfc3339_millis(&skew), "2026-10-17T22:26:01.000Z");

        assert!(parse_date_str_at("Not a date", now).is_none());
        assert!(parse_date_str_at("Oct 17 22:26:01 trailing", now).is_none());
        assert!(parse_date_str_at("Oct 32 00:00:00", now).is_none());
        assert!(
            parse_date_str_at("Tue, 01 Jul 2025 10:00:00 +0000", now)
                .unwrap()
                .1
                != DateEnc::BsdSyslog
        );
    }

    /// #1504: vsftpd `xferlog` stamps the transfer as ctime (`Www Mmm dd
    /// hh:mm:ss yyyy`). That string missed every encoding, so the column
    /// mapped `keyword` and a time-range filter matched nothing. The weekday
    /// has to be the real weekday of the date; a year-less RFC 3164 stamp
    /// stays on the BSD path.
    #[test]
    fn xferlog_asctime_stamps_are_dates_and_a_wrong_weekday_is_not() {
        let (dt, enc) = parse_date_str("Fri Oct 16 09:06:58 2026").unwrap();
        assert_eq!(enc, DateEnc::Asctime);
        assert_eq!(to_rfc3339_millis(&dt), "2026-10-16T09:06:58.000Z");

        let (padded, enc) = parse_date_str("Mon Oct  5 09:06:58 2026").unwrap();
        assert_eq!(enc, DateEnc::Asctime);
        assert_eq!(to_rfc3339_millis(&padded), "2026-10-05T09:06:58.000Z");

        // Oct 16 2026 is a Friday. Saturday is a different day.
        assert!(parse_date_str("Sat Oct 16 09:06:58 2026").is_none());
        // RFC 3164 has no weekday and no year. It must not become asctime.
        let now = at(2026, 10, 17, 23, 0, 0);
        let (_, enc) = parse_date_str_at("Oct 16 09:06:58", now).unwrap();
        assert_eq!(enc, DateEnc::BsdSyslog);
    }
}
