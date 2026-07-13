//! ES-compatible date parsing and date-math resolution for range bounds.
//!
//! Elasticsearch resolves range-query date bounds in three steps:
//!
//! 1. **Parse** the value with the field's format (default
//!    `strict_date_optional_time||epoch_millis`, overridable per-query via the
//!    range clause's `format` parameter).  Partial dates leave trailing
//!    components unset.
//! 2. **Fill** missing components.  The round-*down* parser fills every
//!    missing field with its minimum.  The round-*up* parser mirrors ES's
//!    `JavaDateFormatter` round-up defaults: `month → 1`, `day → 1`,
//!    `hour → 23`, `minute → 59`, `second → 59`, `milli → 999`.  (So
//!    `lte: "2026-02"` covers up to `2026-02-01T23:59:59.999` — the first
//!    *day*, not the whole month.  Verified against live ES 8.13.4 and pinned
//!    by the `500_date_range.yml` conformance tests.)
//! 3. **Date math** — `now+1d/M` or `<anchor>||+1M/d`.  The anchor parses
//!    with *min* fill (no implicit round-up; verified live: `gt
//!    "2026-02-14||+1d"` behaves exactly like `gt "2026-02-14T00:00:00||+1d"`),
//!    then each `+N<unit>` / `-N<unit>` applies calendar-aware, and `/<unit>`
//!    rounds — down to the start of the unit, or (for round-up bounds) to the
//!    last millisecond of the unit.
//!
//! Round-up applies to `lte` (include the whole covered interval) and `gt`
//! (exclude the whole covered interval); `gte` and `lt` round down.
//!
//! Everything here works on UTC epoch milliseconds (ES date resolution).

use chrono::{Datelike, Duration, Months, NaiveDate, NaiveDateTime, Timelike};

// ─────────────────────────────────────────────────────────────────────────────
// Errors
// ─────────────────────────────────────────────────────────────────────────────

/// Why a bound could not be resolved as a date.
#[derive(Debug, PartialEq)]
pub enum DateResolveError {
    /// The `format` parameter itself is invalid (unknown pattern letter).
    /// Carries the offending letter; the caller formats the full ES message
    /// (`Invalid format: [banana]: Unknown pattern letter: b`).
    UnknownPatternLetter(char),
    /// The value failed to parse under an *explicit* `format`.  Carries the
    /// original value text (the caller knows the format string).
    UnparseableValue(String),
    /// The date-math suffix contains an unsupported operator/unit.  Carries
    /// the math substring for ES's
    /// `operator not supported for date math [-5ms]` message.
    BadDateMath(String),
}

// ─────────────────────────────────────────────────────────────────────────────
// Parsed (possibly partial) date
// ─────────────────────────────────────────────────────────────────────────────

/// A parsed date with per-component presence.  Missing components are filled
/// according to the round mode when converting to an instant.
#[derive(Debug, Default, Clone)]
struct DateParts {
    /// Year (default 1970 for time-only formats, mirroring java.time's epoch
    /// base date).
    year: Option<i32>,
    month: Option<u32>,
    day: Option<u32>,
    hour: Option<u32>,
    minute: Option<u32>,
    second: Option<u32>,
    milli: Option<u32>,
    /// UTC offset in seconds (parsed from `Z` / `±hh[:mm]`).  `None` → UTC.
    tz_secs: Option<i32>,
}

impl DateParts {
    /// Resolve to epoch milliseconds.  `round_up` selects the ES round-up
    /// fill for missing fields (month/day → 1, time-of-day → max).
    fn to_epoch_ms(&self, round_up: bool) -> Option<i64> {
        let year = self.year.unwrap_or(1970);
        let month = self.month.unwrap_or(1);
        let day = self.day.unwrap_or(1);
        let (hour, minute, second, milli) = if round_up {
            (
                self.hour.unwrap_or(23),
                self.minute.unwrap_or(59),
                self.second.unwrap_or(59),
                self.milli.unwrap_or(999),
            )
        } else {
            (
                self.hour.unwrap_or(0),
                self.minute.unwrap_or(0),
                self.second.unwrap_or(0),
                self.milli.unwrap_or(0),
            )
        };
        let dt = NaiveDate::from_ymd_opt(year, month, day)?
            .and_hms_milli_opt(hour, minute, second, milli)?;
        let ms = dt.and_utc().timestamp_millis();
        Some(ms - i64::from(self.tz_secs.unwrap_or(0)) * 1000)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Format compilation
// ─────────────────────────────────────────────────────────────────────────────

/// One member of a (possibly `||`-joined) date format list.
#[derive(Debug, Clone)]
pub enum DateFmt {
    EpochMillis,
    EpochSecond,
    /// `strict_date_optional_time` / `date_optional_time` / `iso8601` — the
    /// partial ISO parser (also the default when no `format` is given).
    IsoOptionalTime,
    /// A compiled Java-style pattern (`dd/MM/yyyy`, `uuuu`, `basic_date`, …).
    Pattern(Vec<PatTok>),
}

/// One token of a compiled Java date pattern.
#[derive(Debug, Clone, PartialEq)]
pub enum PatTok {
    /// Year; payload = minimum digit count (`yyyy` → 4).
    Year(usize),
    /// Numeric month; payload = exact-width flag (`MM` → true, `M` → false).
    Month { two_digit: bool },
    /// Text month name (`MMM`/`MMMM`) — English abbreviations/full names.
    MonthName,
    Day { two_digit: bool },
    Hour { two_digit: bool },
    Minute { two_digit: bool },
    Second { two_digit: bool },
    /// Fractional seconds (`S+`); stores as milliseconds.
    Fraction,
    /// `Z`/`X`/`x`/`z` — accepts `Z` or `±hh[:mm]` / `±hhmm` (also `UTC`/`GMT`).
    TzOffset,
    /// Day-of-week name (`E`/`e`) — consumed and discarded.
    WeekdayName,
    /// AM/PM marker (`a`); shifts a 12-hour value.
    AmPm,
    /// Verbatim text that must match exactly.
    Literal(String),
    /// A *valid* Java pattern letter this engine does not implement (e.g.
    /// week-of-year `w`).  Compiling succeeds (ES accepts the format); any
    /// value parsed against it fails, producing ES's `failed to parse date
    /// field` error rather than an invalid-format error.
    Unsupported,
}

/// Java pattern letters that are valid in `java.time.format.DateTimeFormatter`
/// patterns.  Anything alphabetic outside this set produces
/// `Unknown pattern letter: <c>` (ES's `Invalid format` 400).
const VALID_JAVA_PATTERN_LETTERS: &str = "GuyDMLdgQqYwWEecFahKkHmsSAnNVvzOXxZpB";

/// Compile an ES `format` string (possibly `||`-joined) into a format list.
pub fn compile_formats(format: &str) -> Result<Vec<DateFmt>, DateResolveError> {
    format.split("||").map(compile_one_format).collect()
}

fn compile_one_format(name: &str) -> Result<DateFmt, DateResolveError> {
    // Named builtins first (both strict_ and lenient joda names).
    let pattern: &str = match name {
        "epoch_millis" => return Ok(DateFmt::EpochMillis),
        "epoch_second" => return Ok(DateFmt::EpochSecond),
        "strict_date_optional_time"
        | "date_optional_time"
        | "strict_date_optional_time_nanos"
        | "date_optional_time_nanos"
        | "iso8601" => return Ok(DateFmt::IsoOptionalTime),
        "basic_date" => "yyyyMMdd",
        "basic_date_time" => "yyyyMMdd'T'HHmmss.SSSXX",
        "basic_date_time_no_millis" => "yyyyMMdd'T'HHmmssXX",
        "date" | "strict_date" | "year_month_day" | "strict_year_month_day" => "yyyy-MM-dd",
        "year" | "strict_year" => "yyyy",
        "year_month" | "strict_year_month" => "yyyy-MM",
        "date_time" | "strict_date_time" => "yyyy-MM-dd'T'HH:mm:ss.SSSXX",
        "date_time_no_millis" | "strict_date_time_no_millis" => "yyyy-MM-dd'T'HH:mm:ssXX",
        "date_hour_minute_second" | "strict_date_hour_minute_second" => "yyyy-MM-dd'T'HH:mm:ss",
        "date_hour_minute_second_millis"
        | "strict_date_hour_minute_second_millis"
        | "date_hour_minute_second_fraction"
        | "strict_date_hour_minute_second_fraction" => "yyyy-MM-dd'T'HH:mm:ss.SSS",
        "date_hour_minute" | "strict_date_hour_minute" => "yyyy-MM-dd'T'HH:mm",
        "hour_minute_second" | "strict_hour_minute_second" => "HH:mm:ss",
        "rfc3339" | "rfc3339_lenient" => return Ok(DateFmt::IsoOptionalTime),
        other => other,
    };
    compile_pattern(pattern).map(DateFmt::Pattern)
}

/// Compile a Java-style date pattern into tokens.
fn compile_pattern(pattern: &str) -> Result<Vec<PatTok>, DateResolveError> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut toks: Vec<PatTok> = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            // Quoted literal ('' = escaped quote).
            let mut lit = String::new();
            i += 1;
            loop {
                if i >= chars.len() {
                    break; // Unterminated quote — treat gathered text as literal.
                }
                if chars[i] == '\'' {
                    if i + 1 < chars.len() && chars[i + 1] == '\'' {
                        lit.push('\'');
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                lit.push(chars[i]);
                i += 1;
            }
            if lit.is_empty() {
                lit.push('\''); // '' outside text = literal quote
            }
            push_literal(&mut toks, &lit);
            continue;
        }
        if c.is_ascii_alphabetic() {
            if !VALID_JAVA_PATTERN_LETTERS.contains(c) {
                return Err(DateResolveError::UnknownPatternLetter(c));
            }
            let mut run = 1usize;
            while i + run < chars.len() && chars[i + run] == c {
                run += 1;
            }
            i += run;
            let tok = match c {
                'y' | 'u' | 'Y' => PatTok::Year(run.min(4)),
                'M' | 'L' => {
                    if run >= 3 {
                        PatTok::MonthName
                    } else {
                        PatTok::Month { two_digit: run == 2 }
                    }
                }
                'd' => PatTok::Day { two_digit: run >= 2 },
                'H' | 'k' => PatTok::Hour { two_digit: run >= 2 },
                'h' | 'K' => PatTok::Hour { two_digit: run >= 2 },
                'm' => PatTok::Minute { two_digit: run >= 2 },
                's' => PatTok::Second { two_digit: run >= 2 },
                'S' | 'n' | 'A' | 'N' => PatTok::Fraction,
                'Z' | 'X' | 'x' | 'z' | 'V' | 'O' | 'v' => PatTok::TzOffset,
                'E' | 'e' | 'c' => PatTok::WeekdayName,
                'a' | 'B' => PatTok::AmPm,
                // Valid Java letters without an implementation here (era,
                // week-of-year, day-of-year, quarter, …).
                _ => PatTok::Unsupported,
            };
            toks.push(tok);
            continue;
        }
        // Separator / literal character.
        push_literal(&mut toks, &c.to_string());
        i += 1;
    }
    Ok(toks)
}

fn push_literal(toks: &mut Vec<PatTok>, text: &str) {
    if let Some(PatTok::Literal(prev)) = toks.last_mut() {
        prev.push_str(text);
    } else {
        toks.push(PatTok::Literal(text.to_string()));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Value parsing
// ─────────────────────────────────────────────────────────────────────────────

/// Parse a value against one compiled format.  Returns epoch-ms with the
/// round mode applied to missing components.
fn parse_with_format(fmt: &DateFmt, value: &str, round_up: bool) -> Option<i64> {
    match fmt {
        DateFmt::EpochMillis => {
            let v: i64 = value.parse().ok()?;
            Some(v)
        }
        DateFmt::EpochSecond => {
            let v: i64 = value.parse().ok()?;
            v.checked_mul(1000)
        }
        DateFmt::IsoOptionalTime => parse_iso_partial(value)?.to_epoch_ms(round_up),
        DateFmt::Pattern(toks) => parse_pattern(toks, value)?.to_epoch_ms(round_up),
    }
}

/// Parse a (possibly partial) strict ISO-8601 date:
/// `yyyy[-MM[-dd['T'HH[:mm[:ss[.SSS…]]][tz]]]]` where `tz` is `Z` or
/// `±hh[:mm]` / `±hhmm`.  Trailing input → fail.
fn parse_iso_partial(s: &str) -> Option<DateParts> {
    let b = s.as_bytes();
    let mut i = 0usize;
    let mut parts = DateParts::default();

    // Year: optional sign + exactly 4 digits (strict_date_optional_time).
    let neg = if b.first() == Some(&b'-') {
        i += 1;
        true
    } else {
        false
    };
    let year_digits = take_digits(b, &mut i, 4, 4)?;
    let mut year: i32 = year_digits.parse().ok()?;
    if neg {
        year = -year;
    }
    parts.year = Some(year);
    if i == b.len() {
        return Some(parts);
    }

    // -MM
    if b[i] != b'-' {
        return None;
    }
    i += 1;
    parts.month = Some(take_digits(b, &mut i, 2, 2)?.parse().ok()?);
    if i == b.len() {
        return Some(parts);
    }

    // -dd
    if b[i] != b'-' {
        return None;
    }
    i += 1;
    parts.day = Some(take_digits(b, &mut i, 2, 2)?.parse().ok()?);
    if i == b.len() {
        return Some(parts);
    }

    // 'T'HH
    if b[i] != b'T' {
        return None;
    }
    i += 1;
    parts.hour = Some(take_digits(b, &mut i, 2, 2)?.parse().ok()?);

    // [:mm[:ss[.fff…]]]
    if i < b.len() && b[i] == b':' {
        i += 1;
        parts.minute = Some(take_digits(b, &mut i, 2, 2)?.parse().ok()?);
        if i < b.len() && b[i] == b':' {
            i += 1;
            parts.second = Some(take_digits(b, &mut i, 2, 2)?.parse().ok()?);
            if i < b.len() && (b[i] == b'.' || b[i] == b',') {
                i += 1;
                let frac = take_digits(b, &mut i, 1, 9)?;
                parts.milli = Some(frac_to_millis(&frac));
            }
        }
    }

    // Optional timezone.
    if i < b.len() {
        let (tz, used) = parse_tz(&s[i..])?;
        parts.tz_secs = Some(tz);
        i += used;
    }
    if i != b.len() {
        return None;
    }
    Some(parts)
}

/// Parse a value against compiled pattern tokens.
fn parse_pattern(toks: &[PatTok], value: &str) -> Option<DateParts> {
    let b = value.as_bytes();
    let mut i = 0usize;
    let mut parts = DateParts::default();
    let mut pm = false;
    let mut has_ampm = false;

    for (t_idx, tok) in toks.iter().enumerate() {
        match tok {
            PatTok::Year(min_digits) => {
                let neg = if b.get(i) == Some(&b'-') {
                    i += 1;
                    true
                } else {
                    false
                };
                // Adjacent numeric fields (`yyyyMMdd`) parse fixed-width,
                // like java.time: when the next token also consumes digits,
                // the year takes exactly its pattern width.
                let numeric_follows = matches!(
                    toks.get(t_idx + 1),
                    Some(
                        PatTok::Month { .. }
                            | PatTok::Day { .. }
                            | PatTok::Hour { .. }
                            | PatTok::Minute { .. }
                            | PatTok::Second { .. }
                            | PatTok::Fraction
                    )
                );
                let max = if numeric_follows { *min_digits } else { 9 };
                let digits = take_digits(b, &mut i, *min_digits, max)?;
                let mut y: i32 = digits.parse().ok()?;
                if neg {
                    y = -y;
                }
                parts.year = Some(y);
            }
            PatTok::Month { two_digit } => {
                let d = take_digits(b, &mut i, if *two_digit { 2 } else { 1 }, 2)?;
                parts.month = Some(d.parse().ok()?);
            }
            PatTok::MonthName => {
                let name = take_alpha(b, &mut i)?;
                parts.month = Some(month_from_name(&name)?);
            }
            PatTok::Day { two_digit } => {
                let d = take_digits(b, &mut i, if *two_digit { 2 } else { 1 }, 2)?;
                parts.day = Some(d.parse().ok()?);
            }
            PatTok::Hour { two_digit } => {
                let d = take_digits(b, &mut i, if *two_digit { 2 } else { 1 }, 2)?;
                parts.hour = Some(d.parse().ok()?);
            }
            PatTok::Minute { two_digit } => {
                let d = take_digits(b, &mut i, if *two_digit { 2 } else { 1 }, 2)?;
                parts.minute = Some(d.parse().ok()?);
            }
            PatTok::Second { two_digit } => {
                let d = take_digits(b, &mut i, if *two_digit { 2 } else { 1 }, 2)?;
                parts.second = Some(d.parse().ok()?);
            }
            PatTok::Fraction => {
                let d = take_digits(b, &mut i, 1, 9)?;
                parts.milli = Some(frac_to_millis(&d));
            }
            PatTok::TzOffset => {
                let (tz, used) = parse_tz(&value[i..])?;
                parts.tz_secs = Some(tz);
                i += used;
            }
            PatTok::WeekdayName => {
                take_alpha(b, &mut i)?;
            }
            PatTok::AmPm => {
                let a = take_alpha(b, &mut i)?;
                has_ampm = true;
                match a.to_ascii_lowercase().as_str() {
                    "am" => pm = false,
                    "pm" => pm = true,
                    _ => return None,
                }
            }
            PatTok::Literal(lit) => {
                let lb = lit.as_bytes();
                if b.len() < i + lb.len() || &b[i..i + lb.len()] != lb {
                    return None;
                }
                i += lb.len();
            }
            PatTok::Unsupported => return None,
        }
    }
    if i != b.len() {
        return None; // ES: "unparsed text found at index N"
    }
    if has_ampm && pm {
        if let Some(h) = parts.hour {
            if h < 12 {
                parts.hour = Some(h + 12);
            }
        }
    }
    Some(parts)
}

fn take_digits(b: &[u8], i: &mut usize, min: usize, max: usize) -> Option<String> {
    let start = *i;
    while *i < b.len() && b[*i].is_ascii_digit() && *i - start < max {
        *i += 1;
    }
    if *i - start < min {
        return None;
    }
    Some(String::from_utf8_lossy(&b[start..*i]).into_owned())
}

fn take_alpha(b: &[u8], i: &mut usize) -> Option<String> {
    let start = *i;
    while *i < b.len() && b[*i].is_ascii_alphabetic() {
        *i += 1;
    }
    if *i == start {
        return None;
    }
    Some(String::from_utf8_lossy(&b[start..*i]).into_owned())
}

fn frac_to_millis(digits: &str) -> u32 {
    // ".5" → 500 ms, ".123456789" → 123 ms.
    let padded = format!("{:0<3}", &digits[..digits.len().min(3)]);
    padded.parse().unwrap_or(0)
}

fn month_from_name(name: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    let lc = name.to_ascii_lowercase();
    MONTHS
        .iter()
        .position(|m| *m == lc || m[..3] == lc)
        .map(|p| p as u32 + 1)
}

/// Parse a timezone suffix; returns (offset seconds, bytes consumed).
fn parse_tz(s: &str) -> Option<(i32, usize)> {
    let b = s.as_bytes();
    if b.is_empty() {
        return None;
    }
    if b[0] == b'Z' {
        return Some((0, 1));
    }
    if s.starts_with("UTC") || s.starts_with("GMT") {
        return Some((0, 3));
    }
    let sign: i32 = match b[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let mut i = 1usize;
    let hh = take_digits(b, &mut i, 2, 2)?;
    let hours: i32 = hh.parse().ok()?;
    let mut minutes = 0i32;
    if i < b.len() {
        if b[i] == b':' {
            i += 1;
            minutes = take_digits(b, &mut i, 2, 2)?.parse().ok()?;
        } else if b[i].is_ascii_digit() {
            minutes = take_digits(b, &mut i, 2, 2)?.parse().ok()?;
        }
    }
    Some((sign * (hours * 3600 + minutes * 60), i))
}

// ─────────────────────────────────────────────────────────────────────────────
// Date math
// ─────────────────────────────────────────────────────────────────────────────

/// Apply an ES date-math suffix (`+1M`, `-2w/d`, `/M`, chains thereof) to a
/// base instant.  `round_up` picks the rounding direction for `/unit`.
///
/// Errors carry the *whole* math substring, mirroring ES's
/// `operator not supported for date math [-5ms]`.
pub fn apply_date_math(base_ms: i64, math: &str, round_up: bool) -> Result<i64, DateResolveError> {
    let mut dt = ms_to_naive(base_ms).ok_or_else(|| DateResolveError::BadDateMath(math.into()))?;
    let b = math.as_bytes();
    let mut i = 0usize;
    let err = || DateResolveError::BadDateMath(math.to_string());

    while i < b.len() {
        match b[i] {
            b'/' => {
                i += 1;
                if i >= b.len() {
                    return Err(err());
                }
                let unit = b[i] as char;
                i += 1;
                dt = round_naive(dt, unit, round_up).ok_or_else(err)?;
            }
            b'+' | b'-' => {
                let sign: i64 = if b[i] == b'+' { 1 } else { -1 };
                i += 1;
                let start = i;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                let n: i64 = if i == start {
                    1 // `now+y` == `now+1y`
                } else {
                    std::str::from_utf8(&b[start..i])
                        .ok()
                        .and_then(|s| s.parse().ok())
                        .ok_or_else(err)?
                };
                if i >= b.len() {
                    return Err(err());
                }
                let unit = b[i] as char;
                i += 1;
                // A trailing 's' after 'm' would mean the (unsupported in
                // date math) `ms` unit — the next loop iteration rejects the
                // stray 's' as an operator, matching ES.
                dt = add_unit(dt, sign * n, unit).ok_or_else(err)?;
            }
            _ => return Err(err()),
        }
    }
    Ok(dt.and_utc().timestamp_millis())
}

fn ms_to_naive(ms: i64) -> Option<NaiveDateTime> {
    chrono::DateTime::from_timestamp_millis(ms).map(|dt| dt.naive_utc())
}

fn add_unit(dt: NaiveDateTime, n: i64, unit: char) -> Option<NaiveDateTime> {
    match unit {
        'y' => shift_months(dt, n.checked_mul(12)?),
        'M' => shift_months(dt, n),
        'w' => dt.checked_add_signed(Duration::weeks(n)),
        'd' => dt.checked_add_signed(Duration::days(n)),
        'h' | 'H' => dt.checked_add_signed(Duration::hours(n)),
        'm' => dt.checked_add_signed(Duration::minutes(n)),
        's' => dt.checked_add_signed(Duration::seconds(n)),
        _ => None,
    }
}

fn shift_months(dt: NaiveDateTime, n: i64) -> Option<NaiveDateTime> {
    let months = u32::try_from(n.unsigned_abs()).ok()?;
    if n >= 0 {
        dt.checked_add_months(Months::new(months))
    } else {
        dt.checked_sub_months(Months::new(months))
    }
}

/// Round an instant to `unit`.  Round-down → first millisecond of the unit;
/// round-up → last millisecond (ES range semantics).
fn round_naive(dt: NaiveDateTime, unit: char, round_up: bool) -> Option<NaiveDateTime> {
    let floor: NaiveDateTime = match unit {
        'y' => NaiveDate::from_ymd_opt(dt.year(), 1, 1)?.and_hms_opt(0, 0, 0)?,
        'M' => NaiveDate::from_ymd_opt(dt.year(), dt.month(), 1)?.and_hms_opt(0, 0, 0)?,
        'w' => {
            // ISO weeks start on Monday (java.time / ES Rounding).
            let days_back = dt.weekday().num_days_from_monday() as i64;
            (dt.date() - Duration::days(days_back)).and_hms_opt(0, 0, 0)?
        }
        'd' => dt.date().and_hms_opt(0, 0, 0)?,
        'h' | 'H' => dt.date().and_hms_opt(dt.hour(), 0, 0)?,
        'm' => dt.date().and_hms_opt(dt.hour(), dt.minute(), 0)?,
        's' => dt.date().and_hms_opt(dt.hour(), dt.minute(), dt.second())?,
        _ => return None,
    };
    if !round_up {
        return Some(floor);
    }
    let next: NaiveDateTime = match unit {
        'y' => shift_months(floor, 12)?,
        'M' => shift_months(floor, 1)?,
        'w' => floor.checked_add_signed(Duration::weeks(1))?,
        'd' => floor.checked_add_signed(Duration::days(1))?,
        'h' | 'H' => floor.checked_add_signed(Duration::hours(1))?,
        'm' => floor.checked_add_signed(Duration::minutes(1))?,
        's' => floor.checked_add_signed(Duration::seconds(1))?,
        _ => return None,
    };
    next.checked_sub_signed(Duration::milliseconds(1))
}

// ─────────────────────────────────────────────────────────────────────────────
// Bound resolution (public entry)
// ─────────────────────────────────────────────────────────────────────────────

/// Format an epoch-ms instant as the canonical ISO string the engine's
/// comparators parse back via `parse_date_ms`.
fn ms_to_iso(ms: i64) -> Option<String> {
    let dt = chrono::DateTime::from_timestamp_millis(ms)?;
    Some(dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

/// Resolve one range bound string to a canonical ISO instant.
///
/// * `round_up` — true for `lte` / `gt` (ES rounds those bounds up).
/// * `formats`  — the compiled explicit `format` list, or `None` for the
///   default (`strict_date_optional_time||epoch_millis`, where the epoch half
///   is left to the engine's numeric comparator).
///
/// Returns:
/// * `Ok(Some(iso))` — the bound resolved as a date; substitute it.
/// * `Ok(None)` — not date-shaped under the *default* format; leave the bound
///   unchanged (keyword / numeric ranges must keep their semantics — the
///   parser has no mapping information).
/// * `Err(…)` — unparseable under an *explicit* format, or malformed date
///   math (both are hard 400s in ES regardless of field type).
pub fn resolve_date_bound_str(
    value: &str,
    round_up: bool,
    formats: Option<&[DateFmt]>,
) -> Result<Option<String>, DateResolveError> {
    let v = value.trim();

    // `now`-anchored math (never subject to `format`).
    if let Some(math) = v.strip_prefix("now") {
        let base = chrono::Utc::now().timestamp_millis();
        let ms = apply_date_math(base, math, round_up)?;
        return Ok(ms_to_iso(ms));
    }

    // `<anchor>||<math>` — the anchor parses with MIN fill (no implicit
    // round-up; verified live against ES 8.13.4), then math applies with
    // per-operator rounding.
    if let Some((anchor, math)) = v.split_once("||") {
        let anchor_ms = parse_anchor_min(anchor, formats);
        let Some(anchor_ms) = anchor_ms else {
            // A `||` value is unambiguously date math — unparseable anchors
            // are hard errors (ES: failed to parse date field), even under
            // the default format.
            return Err(DateResolveError::UnparseableValue(v.to_string()));
        };
        let ms = apply_date_math(anchor_ms, math, round_up)?;
        return Ok(ms_to_iso(ms));
    }

    // Plain value.
    match formats {
        Some(fmts) => {
            for f in fmts {
                if let Some(ms) = parse_with_format(f, v, round_up) {
                    return Ok(ms_to_iso(ms));
                }
            }
            Err(DateResolveError::UnparseableValue(v.to_string()))
        }
        None => {
            // Default format: only the ISO half rewrites; anything else
            // (bare numbers, keyword data) keeps its existing comparator
            // semantics.
            match parse_iso_partial(v).and_then(|p| p.to_epoch_ms(round_up)) {
                Some(ms) => Ok(ms_to_iso(ms)),
                None => Ok(None),
            }
        }
    }
}

/// Parse a date-math anchor with min fill under `formats` (or the default).
fn parse_anchor_min(anchor: &str, formats: Option<&[DateFmt]>) -> Option<i64> {
    match formats {
        Some(fmts) => fmts
            .iter()
            .find_map(|f| parse_with_format(f, anchor, false)),
        None => {
            // Default `strict_date_optional_time||epoch_millis`.
            if let Some(p) = parse_iso_partial(anchor) {
                return p.to_epoch_ms(false);
            }
            anchor.parse::<i64>().ok()
        }
    }
}

/// Resolve a numeric range bound under an *explicit* format list.
///
/// ES stringifies numbers when the field/query has a non-epoch date format
/// (`gte: 1500, format: "uuuu"` parses "1500" as a year); epoch formats scale
/// numerically.  Returns the same tri-state as [`resolve_date_bound_str`].
pub fn resolve_date_bound_num(
    value: &serde_json::Number,
    round_up: bool,
    formats: &[DateFmt],
) -> Result<Option<String>, DateResolveError> {
    for f in formats {
        match f {
            DateFmt::EpochMillis => {
                if let Some(ms) = value.as_i64() {
                    return Ok(ms_to_iso(ms));
                }
            }
            DateFmt::EpochSecond => {
                if let Some(s) = value.as_i64() {
                    return Ok(ms_to_iso(s.checked_mul(1000).ok_or_else(|| {
                        DateResolveError::UnparseableValue(value.to_string())
                    })?));
                }
            }
            other => {
                let s = value.to_string();
                if let Some(ms) = parse_with_format(other, &s, round_up) {
                    return Ok(ms_to_iso(ms));
                }
            }
        }
    }
    Err(DateResolveError::UnparseableValue(value.to_string()))
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn iso(v: &str, up: bool) -> String {
        resolve_date_bound_str(v, up, None).unwrap().unwrap()
    }

    #[test]
    fn partial_month_fills() {
        // gte/lt (round down): first ms of the parsed fields, min fill.
        assert_eq!(iso("2026-02", false), "2026-02-01T00:00:00.000Z");
        // lte/gt (round up): ES round-up parser fills day → 1, time → max.
        assert_eq!(iso("2026-02", true), "2026-02-01T23:59:59.999Z");
    }

    #[test]
    fn partial_year_fills() {
        assert_eq!(iso("2026", false), "2026-01-01T00:00:00.000Z");
        assert_eq!(iso("2026", true), "2026-01-01T23:59:59.999Z");
    }

    #[test]
    fn day_partial_fills() {
        assert_eq!(iso("2026-02-15", false), "2026-02-15T00:00:00.000Z");
        assert_eq!(iso("2026-02-15", true), "2026-02-15T23:59:59.999Z");
    }

    #[test]
    fn hour_minute_partials() {
        assert_eq!(iso("2026-02-15T08", true), "2026-02-15T08:59:59.999Z");
        assert_eq!(iso("2026-02-15T08:30", true), "2026-02-15T08:30:59.999Z");
        assert_eq!(iso("2026-02-15T08:30:05", true), "2026-02-15T08:30:05.999Z");
        assert_eq!(
            iso("2026-02-15T08:30:05.123", true),
            "2026-02-15T08:30:05.123Z"
        );
    }

    #[test]
    fn tz_offsets() {
        assert_eq!(
            iso("2026-02-15T08:30:00+02:00", false),
            "2026-02-15T06:30:00.000Z"
        );
        assert_eq!(
            iso("2026-02-15T08:30:00Z", false),
            "2026-02-15T08:30:00.000Z"
        );
        assert_eq!(
            iso("2026-02-15T08:30:00-0330", false),
            "2026-02-15T12:00:00.000Z"
        );
    }

    #[test]
    fn anchored_math_min_fill_then_round() {
        // Anchor gets min fill even for round-up bounds (live-verified).
        assert_eq!(iso("2026-02-14||+1d", true), "2026-02-15T00:00:00.000Z");
        // /M rounding is true month-end for round-up …
        assert_eq!(iso("2026-02-15||/M", true), "2026-02-28T23:59:59.999Z");
        // … and month-start for round-down.
        assert_eq!(iso("2026-02-15||/M", false), "2026-02-01T00:00:00.000Z");
        // +1M/d chains.
        assert_eq!(iso("2026-01-01||+1M/d", true), "2026-02-01T23:59:59.999Z");
        assert_eq!(iso("2026-01-01||+1M/d", false), "2026-02-01T00:00:00.000Z");
        // Month addition clamps at end-of-month like java.time.
        assert_eq!(iso("2026-01-31||+1M", false), "2026-02-28T00:00:00.000Z");
    }

    #[test]
    fn week_rounding_is_monday_based() {
        // 2026-02-15 is a Sunday; ISO week = 02-09 (Mon) .. 02-15.
        assert_eq!(iso("2026-02-15||/w", false), "2026-02-09T00:00:00.000Z");
        assert_eq!(iso("2026-02-15||/w", true), "2026-02-15T23:59:59.999Z");
    }

    #[test]
    fn bad_math_reports_whole_suffix() {
        let e = resolve_date_bound_str("now-5ms", false, None).unwrap_err();
        assert_eq!(e, DateResolveError::BadDateMath("-5ms".into()));
        let e = resolve_date_bound_str("2026-01-01||banana", false, None).unwrap_err();
        assert_eq!(e, DateResolveError::BadDateMath("banana".into()));
    }

    #[test]
    fn bad_anchor_is_hard_error() {
        let e = resolve_date_bound_str("abc||+1d", false, None).unwrap_err();
        assert_eq!(e, DateResolveError::UnparseableValue("abc||+1d".into()));
    }

    #[test]
    fn default_format_leaves_non_dates_alone() {
        assert_eq!(resolve_date_bound_str("apple", false, None), Ok(None));
        assert_eq!(resolve_date_bound_str("123", false, None), Ok(None));
        assert_eq!(
            resolve_date_bound_str("1770000000000", false, None),
            Ok(None)
        );
        // 2-digit / 5-digit numbers are not strict 4-digit years.
        assert_eq!(resolve_date_bound_str("20261", false, None), Ok(None));
    }

    #[test]
    fn explicit_format_parses_and_errors() {
        let fmts = compile_formats("dd/MM/yyyy").unwrap();
        assert_eq!(
            resolve_date_bound_str("15/02/2026", true, Some(&fmts)).unwrap(),
            Some("2026-02-15T23:59:59.999Z".into())
        );
        let e = resolve_date_bound_str("2026-02-03", false, Some(&fmts)).unwrap_err();
        assert_eq!(e, DateResolveError::UnparseableValue("2026-02-03".into()));
    }

    #[test]
    fn format_year_numbers() {
        let fmts = compile_formats("uuuu").unwrap();
        let n = serde_json::Number::from(1500);
        assert_eq!(
            resolve_date_bound_num(&n, false, &fmts).unwrap(),
            Some("1500-01-01T00:00:00.000Z".into())
        );
        assert_eq!(
            resolve_date_bound_num(&n, true, &fmts).unwrap(),
            Some("1500-01-01T23:59:59.999Z".into())
        );
    }

    #[test]
    fn invalid_format_letter() {
        assert_eq!(
            compile_formats("banana").unwrap_err(),
            DateResolveError::UnknownPatternLetter('b')
        );
        // First invalid letter of a || list wins.
        assert_eq!(
            compile_formats("yyyy||bogus").unwrap_err(),
            DateResolveError::UnknownPatternLetter('b')
        );
    }

    #[test]
    fn epoch_formats() {
        let fmts = compile_formats("epoch_millis").unwrap();
        assert_eq!(
            resolve_date_bound_str("1770000000000", false, Some(&fmts)).unwrap(),
            Some("2026-02-02T02:40:00.000Z".into())
        );
        let fmts = compile_formats("epoch_second").unwrap();
        assert_eq!(
            resolve_date_bound_str("1770000000", false, Some(&fmts)).unwrap(),
            Some("2026-02-02T02:40:00.000Z".into())
        );
    }

    #[test]
    fn now_math_rounds() {
        // now/d for a round-up bound ends today (last ms).
        let s = iso("now/d", true);
        assert!(s.ends_with("T23:59:59.999Z"), "{s}");
        let s = iso("now/d", false);
        assert!(s.ends_with("T00:00:00.000Z"), "{s}");
    }

    #[test]
    fn basic_date_named_format() {
        let fmts = compile_formats("basic_date").unwrap();
        assert_eq!(
            resolve_date_bound_str("20260215", true, Some(&fmts)).unwrap(),
            Some("2026-02-15T23:59:59.999Z".into())
        );
    }

    #[test]
    fn strict_date_rejects_partials() {
        let fmts = compile_formats("strict_date").unwrap();
        assert!(resolve_date_bound_str("2026-02", false, Some(&fmts)).is_err());
    }

    #[test]
    fn iso_rejects_trailing_garbage() {
        assert_eq!(resolve_date_bound_str("2026-02-15X", false, None), Ok(None));
        assert_eq!(resolve_date_bound_str("2026-2-15", false, None), Ok(None));
    }
}
