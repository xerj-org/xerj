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

use chrono::{Datelike, Duration, Months, NaiveDate, NaiveDateTime, Timelike, Weekday};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

// ─────────────────────────────────────────────────────────────────────────────
// Errors
// ─────────────────────────────────────────────────────────────────────────────

/// Why a bound could not be resolved as a date.
#[derive(Debug, Clone, PartialEq)]
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
    /// Day-of-year (`D`/`DDD`), the ordinal-date formats' day component.
    /// Takes precedence over `month`/`day` when present.
    day_of_year: Option<u32>,
    /// ISO week-based year (`x` in the built-in week-date formats).  When
    /// present the calendar date comes from (week_year, week, weekday) and
    /// `year`/`month`/`day` are ignored.
    week_year: Option<i32>,
    /// ISO week-of-week-based-year, 1-53 (`w`).
    week: Option<u32>,
    /// ISO day-of-week, 1 = Monday … 7 = Sunday (`e`).
    weekday: Option<u32>,
    hour: Option<u32>,
    minute: Option<u32>,
    second: Option<u32>,
    milli: Option<u32>,
    /// UTC offset in seconds (parsed from `Z` / `±hh[:mm]`).  `None` → UTC.
    tz_secs: Option<i32>,
}

/// Map ISO day-of-week 1-7 (Mon-Sun) to a chrono `Weekday`.
fn iso_weekday(n: u32) -> Option<Weekday> {
    Some(match n {
        1 => Weekday::Mon,
        2 => Weekday::Tue,
        3 => Weekday::Wed,
        4 => Weekday::Thu,
        5 => Weekday::Fri,
        6 => Weekday::Sat,
        7 => Weekday::Sun,
        _ => return None,
    })
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
        // Three mutually exclusive ways to name a calendar day, in the
        // precedence java.time resolves them: ISO week date (week-based
        // year + week + day-of-week), ordinal date (year + day-of-year),
        // then the ordinary year/month/day.
        let date = if let Some(wy) = self.week_year {
            let weekday = match self.weekday {
                Some(d) => iso_weekday(d)?,
                None if round_up => Weekday::Sun,
                None => Weekday::Mon,
            };
            match self.week {
                Some(w) => NaiveDate::from_isoywd_opt(wy, w, weekday)?,
                // A bare `weekyear` (`xxxx`) covers the whole ISO year:
                // round down to week 1, round up to its last week (53 when
                // the year has one, else 52).
                None if round_up => NaiveDate::from_isoywd_opt(wy, 53, weekday)
                    .or_else(|| NaiveDate::from_isoywd_opt(wy, 52, weekday))?,
                None => NaiveDate::from_isoywd_opt(wy, 1, weekday)?,
            }
        } else if let Some(doy) = self.day_of_year {
            NaiveDate::from_yo_opt(year, doy)?
        } else {
            NaiveDate::from_ymd_opt(year, month, day)?
        };
        let dt = date.and_hms_milli_opt(hour, minute, second, milli)?;
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
    Month {
        two_digit: bool,
    },
    /// Text month name (`MMM`/`MMMM`) — English abbreviations/full names.
    MonthName,
    Day {
        two_digit: bool,
    },
    /// Day-of-year (`D`/`DDD`); payload = minimum digit count.  Drives the
    /// ordinal-date formats (`ordinal_date` = `yyyy-DDD`).
    DayOfYear(usize),
    /// ISO week-based year (`x` inside the built-in week-date formats);
    /// payload = exact digit count.
    WeekYear(usize),
    /// ISO week-of-week-based-year (`w`); payload = exact-width flag.
    Week {
        two_digit: bool,
    },
    /// Numeric ISO day-of-week, 1 = Monday … 7 = Sunday (`e`/`ee` inside the
    /// built-in week-date formats).
    DayOfWeekNum,
    Hour {
        two_digit: bool,
    },
    Minute {
        two_digit: bool,
    },
    Second {
        two_digit: bool,
    },
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
    /// A `[...]` optional section (java.time's `optionalStart`/`optionalEnd`).
    /// Matches if the whole section matches; otherwise it is skipped.
    Optional(Vec<PatTok>),
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

/// How many distinct format strings the compile cache will hold.  Format
/// strings come from mappings and from range-query `format` parameters, so
/// the key space is attacker-influenced; past this many entries the cache
/// stops growing and later formats simply compile every time (correct, just
/// not cached).
const FORMAT_CACHE_CAP: usize = 1024;

type FormatCacheEntry = Result<Arc<Vec<DateFmt>>, DateResolveError>;

fn format_cache() -> &'static RwLock<HashMap<String, FormatCacheEntry>> {
    static CACHE: OnceLock<RwLock<HashMap<String, FormatCacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// [`compile_formats`], memoised on the format string.
///
/// Ingest recompiles a field's declared `format` once per *value*: a bulk of
/// 10k docs with one date field re-tokenises the same pattern 10k times.  The
/// format is a property of the mapping, not of the value, so the compiled
/// result is cached and handed out behind an `Arc`.  Compilation *failures*
/// are cached too — a bad format is equally hot and equally deterministic.
pub fn compile_formats_cached(format: &str) -> FormatCacheEntry {
    if let Ok(cache) = format_cache().read() {
        if let Some(hit) = cache.get(format) {
            return hit.clone();
        }
    }
    let compiled = compile_formats(format).map(Arc::new);
    if let Ok(mut cache) = format_cache().write() {
        if cache.len() < FORMAT_CACHE_CAP {
            cache.insert(format.to_string(), compiled.clone());
        }
    }
    compiled
}

/// Is `value` acceptable for a date field declaring `format`?
///
/// THE single ingest-time date-validation predicate.  Both ingest paths must
/// call it and nothing else:
///
/// * `ignore_malformed: true` — a value that fails is dropped from the doc
///   and listed in `_ignored`.
/// * `ignore_malformed: false` — a value that fails rejects the whole
///   document with `document_parsing_exception`.
///
/// They used to be two independent implementations, and the strict one was
/// the *laxer* of the two: it accepted any string under any named format, so
/// turning `ignore_malformed` off — asking for stricter handling — made the
/// engine accept more. Sharing one predicate is what keeps that from
/// happening again; if you are about to write a second one, don't.
///
/// A format string that does not compile returns `false` (fail closed): an
/// unresolvable format is not evidence the value is fine, only that we
/// cannot tell.  Real ES rejects such a format when the mapping is created,
/// so no document should ever reach this with one.
pub fn date_value_valid_with_format(value: &serde_json::Value, format: &str) -> bool {
    match compile_formats_cached(format) {
        Ok(formats) => date_value_matches_formats(value, &formats),
        Err(_) => false,
    }
}

/// Does `value` parse as a valid date under any of the compiled `formats`?
/// This is the ingest-time validation check (`ignore_malformed` decides
/// whether a field that fails this gets dropped or the whole doc rejected),
/// so it must match ES's actual acceptance rules precisely:
///
/// * `null` is always valid — ES treats it as "no value", not malformed
///   input, uniformly across field types.
/// * A JSON number is stringified and matched the same way a string would
///   be, against every member of a `||`-joined format list — mirroring
///   `DateFieldMapper`, which reads the value via the parser's `text()`
///   (stringifying a numeric token) before handing it to the declared
///   formatter. A number is valid input for ANY format whose textual form
///   it matches, not just `epoch_millis`/`epoch_second`.
/// * A string is valid whenever ANY member of the format list parses it —
///   reusing [`parse_with_format`], the same parser
///   [`resolve_date_bound_str`] uses for range-query bounds, so named
///   formats (`strict_date_time`, `date_optional_time`, ...) resolve to
///   their real pattern instead of being matched as literal text. No
///   whitespace trimming: ES hands the value to the formatter as-is.
pub fn date_value_matches_formats(value: &serde_json::Value, formats: &[DateFmt]) -> bool {
    match value {
        // ES treats JSON `null` as "no value" uniformly across field types
        // (not malformed input), so it's never recorded in `_ignored`.
        serde_json::Value::Null => true,
        // ES's DateFieldMapper reads the value via the JSON parser's
        // `text()`, which stringifies a numeric token before handing it to
        // the declared formatter — a number is valid input for ANY format
        // whose textual form it can match, not just epoch_millis/
        // epoch_second. `basic_date` (`yyyyMMdd`) + the number `20240101`
        // stringifies to "20240101" and parses cleanly; gating numeric
        // input on epoch formats specifically (as this used to) silently
        // dropped exactly that shape.
        serde_json::Value::Number(n) => {
            let s = n.to_string();
            formats.iter().any(|f| value_matches_format(f, &s))
        }
        // No trim: ES does not strip leading/trailing whitespace before
        // handing the value to the declared formatter, so a padded value
        // that would fail there must fail here too.
        serde_json::Value::String(s) => formats.iter().any(|f| value_matches_format(f, s)),
        _ => false,
    }
}

/// Validation-only match of one value against one compiled format.
///
/// Strict parse first — the same one range bounds use.  If that fails and the
/// pattern contains a *locale-dependent* text field (`MMM` month name, `E`
/// weekday name), retry with those fields matched as opaque text.
///
/// This engine only knows English month names, but ES honours the mapping's
/// `locale`, so under `format: "E, d MMM yyyy HH:mm:ss Z"` + `locale: "fr"`
/// the value `"mer., 6 déc. 2000 02:55:00 -0800"` is perfectly valid data.
/// Failing it would drop (or reject) documents real ES indexes — the exact
/// class of bug this whole predicate exists to prevent.  So a month name we
/// can't identify is treated as "unverified", NOT as "malformed": everything
/// else in the pattern — the separators, the day, the year, the time, the
/// offset — is still checked, so genuine garbage is still rejected.
///
/// Note this is deliberately different from an *unresolvable format*, which
/// fails closed.  There we know nothing about the value; here we have checked
/// every part of it except which language a word is in.
fn value_matches_format(fmt: &DateFmt, value: &str) -> bool {
    if parse_with_format(fmt, value, false).is_some() {
        return true;
    }
    match fmt {
        DateFmt::Pattern(toks) if has_locale_text(toks) => {
            parse_pattern_with(toks, value, TextMatch::LocaleTolerant)
                .and_then(|p| p.to_epoch_ms(false))
                .is_some()
        }
        _ => false,
    }
}

/// Does this pattern contain a field whose text depends on the locale?
fn has_locale_text(toks: &[PatTok]) -> bool {
    toks.iter()
        .any(|t| matches!(t, PatTok::MonthName | PatTok::WeekdayName))
}

/// Every date format name Elasticsearch ships, lenient and `strict_` alike.
///
/// Exported so tests in other crates can assert the whole set resolves: a
/// name that silently stops resolving becomes a silent data-loss bug on the
/// `ignore_malformed: true` path, and this list is what makes that loud.
pub const ES_NAMED_FORMATS: &[&str] = &[
    "epoch_millis",
    "epoch_second",
    "date_optional_time",
    "strict_date_optional_time",
    "date_optional_time_nanos",
    "strict_date_optional_time_nanos",
    "iso8601",
    "rfc3339",
    "basic_date",
    "strict_basic_date",
    "basic_date_time",
    "strict_basic_date_time",
    "basic_date_time_no_millis",
    "strict_basic_date_time_no_millis",
    "basic_ordinal_date",
    "strict_basic_ordinal_date",
    "basic_ordinal_date_time",
    "strict_basic_ordinal_date_time",
    "basic_ordinal_date_time_no_millis",
    "strict_basic_ordinal_date_time_no_millis",
    "basic_time",
    "strict_basic_time",
    "basic_time_no_millis",
    "strict_basic_time_no_millis",
    "basic_t_time",
    "strict_basic_t_time",
    "basic_t_time_no_millis",
    "strict_basic_t_time_no_millis",
    "basic_week_date",
    "strict_basic_week_date",
    "basic_week_date_time",
    "strict_basic_week_date_time",
    "basic_week_date_time_no_millis",
    "strict_basic_week_date_time_no_millis",
    "date",
    "strict_date",
    "date_hour",
    "strict_date_hour",
    "date_hour_minute",
    "strict_date_hour_minute",
    "date_hour_minute_second",
    "strict_date_hour_minute_second",
    "date_hour_minute_second_fraction",
    "strict_date_hour_minute_second_fraction",
    "date_hour_minute_second_millis",
    "strict_date_hour_minute_second_millis",
    "date_time",
    "strict_date_time",
    "date_time_no_millis",
    "strict_date_time_no_millis",
    "hour",
    "strict_hour",
    "hour_minute",
    "strict_hour_minute",
    "hour_minute_second",
    "strict_hour_minute_second",
    "hour_minute_second_fraction",
    "strict_hour_minute_second_fraction",
    "hour_minute_second_millis",
    "strict_hour_minute_second_millis",
    "ordinal_date",
    "strict_ordinal_date",
    "ordinal_date_time",
    "strict_ordinal_date_time",
    "ordinal_date_time_no_millis",
    "strict_ordinal_date_time_no_millis",
    "time",
    "strict_time",
    "time_no_millis",
    "strict_time_no_millis",
    "t_time",
    "strict_t_time",
    "t_time_no_millis",
    "strict_t_time_no_millis",
    "week_date",
    "strict_week_date",
    "week_date_time",
    "strict_week_date_time",
    "week_date_time_no_millis",
    "strict_week_date_time_no_millis",
    "weekyear",
    "strict_weekyear",
    "weekyear_week",
    "strict_weekyear_week",
    "weekyear_week_day",
    "strict_weekyear_week_day",
    "year",
    "strict_year",
    "year_month",
    "strict_year_month",
    "year_month_day",
    "strict_year_month_day",
];

/// Resolve one ES built-in named date format to its pattern.
///
/// This is the complete list of names Elasticsearch ships (`DateFormatters`),
/// in the Joda spellings ES's own documentation uses.  Every name here MUST
/// resolve: a name that falls through to being compiled as a literal pattern
/// hits an invalid pattern letter (`b` in `basic_time`, `o` in `ordinal_date`,
/// `t` in `time`, …) and the whole format errors out.  Because callers treat
/// an unresolvable format as "this engine cannot validate anything for this
/// field", a missing entry here is not a cosmetic gap — under
/// `ignore_malformed: true` it silently drops every value of the field.
///
/// `strict_` and lenient names map to the same pattern.  Real ES only differs
/// between them in how tolerant it is of missing zero-padding; this engine's
/// pattern parser is strict for both, which is the pre-existing convention
/// (`date` / `strict_date` already shared `yyyy-MM-dd`).
fn builtin_format_pattern(name: &str) -> Option<&'static str> {
    // Strip the `strict_` prefix — every strict variant maps to the same
    // pattern as its lenient twin.
    let base = name.strip_prefix("strict_").unwrap_or(name);
    Some(match base {
        // ── basic (no separators) ────────────────────────────────────────
        // The `.SSS` fraction is bracketed wherever ES's own formatter
        // builds it with `optionalStart()` — `strict_date_time` accepts
        // `2021-05-01T07:10:00Z` with no millis at all.  Requiring it here
        // would reject values real ES indexes.
        "basic_date" => "yyyyMMdd",
        "basic_date_time" => "yyyyMMdd'T'HHmmss[.SSS]XX",
        "basic_date_time_no_millis" => "yyyyMMdd'T'HHmmssXX",
        "basic_ordinal_date" => "yyyyDDD",
        "basic_ordinal_date_time" => "yyyyDDD'T'HHmmss[.SSS]XX",
        "basic_ordinal_date_time_no_millis" => "yyyyDDD'T'HHmmssXX",
        "basic_time" => "HHmmss[.SSS]XX",
        "basic_time_no_millis" => "HHmmssXX",
        "basic_t_time" => "'T'HHmmss[.SSS]XX",
        "basic_t_time_no_millis" => "'T'HHmmssXX",
        "basic_week_date" => "xxxx'W'wwe",
        "basic_week_date_time" => "xxxx'W'wwe'T'HHmmss[.SSS]XX",
        "basic_week_date_time_no_millis" => "xxxx'W'wwe'T'HHmmssXX",
        // ── calendar date / date-time ────────────────────────────────────
        "date" | "year_month_day" => "yyyy-MM-dd",
        "date_hour" => "yyyy-MM-dd'T'HH",
        "date_hour_minute" => "yyyy-MM-dd'T'HH:mm",
        "date_hour_minute_second" => "yyyy-MM-dd'T'HH:mm:ss",
        "date_hour_minute_second_millis" | "date_hour_minute_second_fraction" => {
            "yyyy-MM-dd'T'HH:mm:ss.SSS"
        }
        "date_time" => "yyyy-MM-dd'T'HH:mm:ss[.SSS]XX",
        "date_time_no_millis" => "yyyy-MM-dd'T'HH:mm:ssXX",
        "year" => "yyyy",
        "year_month" => "yyyy-MM",
        // ── time of day ──────────────────────────────────────────────────
        "hour" => "HH",
        "hour_minute" => "HH:mm",
        "hour_minute_second" => "HH:mm:ss",
        "hour_minute_second_millis" | "hour_minute_second_fraction" => "HH:mm:ss.SSS",
        "time" => "HH:mm:ss[.SSS]XX",
        "time_no_millis" => "HH:mm:ssXX",
        "t_time" => "'T'HH:mm:ss[.SSS]XX",
        "t_time_no_millis" => "'T'HH:mm:ssXX",
        // ── ordinal date (day-of-year) ───────────────────────────────────
        "ordinal_date" => "yyyy-DDD",
        "ordinal_date_time" => "yyyy-DDD'T'HH:mm:ss[.SSS]XX",
        "ordinal_date_time_no_millis" => "yyyy-DDD'T'HH:mm:ssXX",
        // ── ISO week date ────────────────────────────────────────────────
        "week_date" | "weekyear_week_day" => "xxxx-'W'ww-e",
        "week_date_time" => "xxxx-'W'ww-e'T'HH:mm:ss[.SSS]XX",
        "week_date_time_no_millis" => "xxxx-'W'ww-e'T'HH:mm:ssXX",
        "weekyear" => "xxxx",
        "weekyear_week" => "xxxx-'W'ww",
        _ => return None,
    })
}

fn compile_one_format(name: &str) -> Result<DateFmt, DateResolveError> {
    // Named builtins first (both strict_ and lenient joda names).
    match name {
        "epoch_millis" => return Ok(DateFmt::EpochMillis),
        "epoch_second" => return Ok(DateFmt::EpochSecond),
        "strict_date_optional_time"
        | "date_optional_time"
        | "strict_date_optional_time_nanos"
        | "date_optional_time_nanos"
        | "iso8601"
        | "rfc3339"
        | "rfc3339_lenient" => return Ok(DateFmt::IsoOptionalTime),
        _ => {}
    }
    if let Some(pattern) = builtin_format_pattern(name) {
        return compile_pattern_in(pattern, PatternDialect::Builtin).map(DateFmt::Pattern);
    }
    // Not a built-in name — treat the string as a user-supplied java.time
    // pattern, exactly as ES does.  An invalid pattern letter errors here
    // (ES: `Invalid format: [banana]: Unknown pattern letter: b`).
    compile_pattern_in(name, PatternDialect::User).map(DateFmt::Pattern)
}

/// Which pattern-letter dialect a pattern string is written in.
#[derive(Clone, Copy, PartialEq)]
enum PatternDialect {
    /// A user-supplied `java.time` pattern from a mapping or a range
    /// query's `format`.  Letters keep their `java.time` meanings (`x` is a
    /// zone offset, `e` is a day-of-week *name*).
    User,
    /// One of the built-in names expanded by [`builtin_format_pattern`].
    /// Those are written in the Joda spellings ES's docs use, where `x` is
    /// the week-based year, `w` the week-of-week-based-year and `e` the
    /// numeric day-of-week.  Confining these meanings to the built-in
    /// dialect keeps user patterns behaving exactly as they did.
    Builtin,
}

/// Compile a Java-style date pattern into tokens.
fn compile_pattern_in(
    pattern: &str,
    dialect: PatternDialect,
) -> Result<Vec<PatTok>, DateResolveError> {
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
        // `[...]` optional section, java.time's optionalStart/optionalEnd.
        // Only the built-in patterns use it; user patterns keep `[` as a
        // literal, exactly as before.
        if c == '[' && dialect == PatternDialect::Builtin {
            let mut depth = 1usize;
            let start = i + 1;
            let mut j = start;
            while j < chars.len() && depth > 0 {
                match chars[j] {
                    '[' => depth += 1,
                    ']' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            let inner: String = chars[start..j.saturating_sub(1)].iter().collect();
            toks.push(PatTok::Optional(compile_pattern_in(&inner, dialect)?));
            i = j;
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
                        PatTok::Month {
                            two_digit: run == 2,
                        }
                    }
                }
                'd' => PatTok::Day {
                    two_digit: run >= 2,
                },
                // Day-of-year: `DDD` (ordinal dates) is exactly 3 digits,
                // `D` accepts 1-3.  Previously unimplemented, so every
                // ordinal-date value failed to parse.
                'D' => PatTok::DayOfYear(run.min(3)),
                // Joda `x` = week-based year in the built-in week-date
                // formats; `java.time` `x` = zone offset everywhere else.
                'x' if dialect == PatternDialect::Builtin => PatTok::WeekYear(run.min(4)),
                'w' if dialect == PatternDialect::Builtin => PatTok::Week {
                    two_digit: run >= 2,
                },
                'e' if dialect == PatternDialect::Builtin => PatTok::DayOfWeekNum,
                'H' | 'k' => PatTok::Hour {
                    two_digit: run >= 2,
                },
                'h' | 'K' => PatTok::Hour {
                    two_digit: run >= 2,
                },
                'm' => PatTok::Minute {
                    two_digit: run >= 2,
                },
                's' => PatTok::Second {
                    two_digit: run >= 2,
                },
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

/// How `MMM`/`E` text fields are matched.
#[derive(Clone, Copy, PartialEq)]
enum TextMatch {
    /// Month names must be English (the only locale this engine knows).
    /// Used for range-bound resolution, which needs a real month number.
    English,
    /// Any alphabetic run (plus an optional trailing `.`) matches, and an
    /// unrecognised month resolves to January.  Validation only — see
    /// [`value_matches_format`].
    LocaleTolerant,
}

/// Parse a value against compiled pattern tokens.
fn parse_pattern(toks: &[PatTok], value: &str) -> Option<DateParts> {
    parse_pattern_with(toks, value, TextMatch::English)
}

fn parse_pattern_with(toks: &[PatTok], value: &str, text: TextMatch) -> Option<DateParts> {
    let mut st = PatState {
        i: 0,
        parts: DateParts::default(),
        pm: false,
        has_ampm: false,
    };
    consume_tokens(toks, value, text, &mut st)?;
    if st.i != value.len() {
        return None; // ES: "unparsed text found at index N"
    }
    if st.has_ampm && st.pm {
        if let Some(h) = st.parts.hour {
            if h < 12 {
                st.parts.hour = Some(h + 12);
            }
        }
    }
    Some(st.parts)
}

/// Parser position plus the fields decoded so far.  Cloned to trial-parse an
/// optional section and rolled back if it doesn't match.
#[derive(Clone)]
struct PatState {
    i: usize,
    parts: DateParts,
    pm: bool,
    has_ampm: bool,
}

/// Consume `toks` from `st.i`.  Returns `None` (leaving `st` unusable) if any
/// token fails to match; callers that need to recover clone `st` first.
fn consume_tokens(toks: &[PatTok], value: &str, text: TextMatch, st: &mut PatState) -> Option<()> {
    let b = value.as_bytes();
    let mut i = st.i;
    let mut parts = st.parts.clone();
    let mut pm = st.pm;
    let mut has_ampm = st.has_ampm;

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
                            | PatTok::DayOfYear(_)
                            | PatTok::Week { .. }
                            | PatTok::DayOfWeekNum
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
                if text == TextMatch::LocaleTolerant {
                    // Consume the word; if it isn't an English month we
                    // can't say which month it is, only that a word is
                    // where a month name belongs. January stands in so the
                    // value still has to be a resolvable date overall.
                    let name = take_locale_text(value, &mut i)?;
                    parts.month = Some(month_from_name(&name).unwrap_or(1));
                } else {
                    let name = take_alpha(b, &mut i)?;
                    parts.month = Some(month_from_name(&name)?);
                }
            }
            PatTok::Day { two_digit } => {
                let d = take_digits(b, &mut i, if *two_digit { 2 } else { 1 }, 2)?;
                parts.day = Some(d.parse().ok()?);
            }
            PatTok::DayOfYear(min_digits) => {
                let d = take_digits(b, &mut i, *min_digits, 3)?;
                let doy: u32 = d.parse().ok()?;
                if !(1..=366).contains(&doy) {
                    return None;
                }
                parts.day_of_year = Some(doy);
            }
            PatTok::WeekYear(digits) => {
                let neg = if b.get(i) == Some(&b'-') {
                    i += 1;
                    true
                } else {
                    false
                };
                let d = take_digits(b, &mut i, *digits, *digits)?;
                let mut wy: i32 = d.parse().ok()?;
                if neg {
                    wy = -wy;
                }
                parts.week_year = Some(wy);
            }
            PatTok::Week { two_digit } => {
                let d = take_digits(b, &mut i, if *two_digit { 2 } else { 1 }, 2)?;
                let w: u32 = d.parse().ok()?;
                if !(1..=53).contains(&w) {
                    return None;
                }
                parts.week = Some(w);
            }
            PatTok::DayOfWeekNum => {
                let d = take_digits(b, &mut i, 1, 1)?;
                let dow: u32 = d.parse().ok()?;
                if !(1..=7).contains(&dow) {
                    return None;
                }
                parts.weekday = Some(dow);
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
                if text == TextMatch::LocaleTolerant {
                    take_locale_text(value, &mut i)?;
                } else {
                    take_alpha(b, &mut i)?;
                }
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
            // `[...]` — try the section, and simply skip it if it does not
            // match. This is what makes a trailing `.SSS` optional in the
            // `*_date_time` formats, where ES's own formatter builds the
            // fraction with `optionalStart()`.
            PatTok::Optional(inner) => {
                let mut trial = PatState {
                    i,
                    parts: parts.clone(),
                    pm,
                    has_ampm,
                };
                if consume_tokens(inner, value, text, &mut trial).is_some() {
                    i = trial.i;
                    parts = trial.parts;
                    pm = trial.pm;
                    has_ampm = trial.has_ampm;
                }
            }
            PatTok::Unsupported => return None,
        }
    }
    st.i = i;
    st.parts = parts;
    st.pm = pm;
    st.has_ampm = has_ampm;
    Some(())
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

/// Consume a locale-agnostic text field: a run of alphabetic characters in
/// any script, plus an optional trailing `.`.  Both are needed for real
/// non-English month/weekday abbreviations — French December is `déc.`, dot
/// included, and `é` is not ASCII.
fn take_locale_text(value: &str, i: &mut usize) -> Option<String> {
    let start = *i;
    for ch in value[*i..].chars() {
        if ch.is_alphabetic() {
            *i += ch.len_utf8();
        } else {
            break;
        }
    }
    if *i == start {
        return None;
    }
    let word = value[start..*i].to_string();
    if value[*i..].starts_with('.') {
        *i += 1;
    }
    Some(word)
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

    // ── date_value_matches_formats (ingest-time ignore_malformed check) ──────
    // Regression coverage for a real bug found live against OpenSearch's UBI
    // sample dashboards: a named strict format like `strict_date_time` was
    // matched as literal text (never resolving to its actual pattern), so
    // every valid ISO string was wrongly rejected, while a bare epoch-millis
    // number was wrongly ACCEPTED under any format, non-epoch included —
    // exactly backwards from real Elasticsearch/OpenSearch behavior
    // (confirmed by replaying the same mapping + documents against a real
    // OpenSearch 2.11.1 node).

    #[test]
    fn named_strict_format_accepts_matching_iso_string() {
        let formats = compile_formats("strict_date_time").unwrap();
        let v = serde_json::json!("2024-01-01T00:00:00.000Z");
        assert!(date_value_matches_formats(&v, &formats));
    }

    #[test]
    fn named_strict_format_rejects_bare_number() {
        // strict_date_time has no epoch_millis in its format list, so a raw
        // number must NOT be accepted — matching real OpenSearch, which
        // ignores this exact value under this exact mapping.
        let formats = compile_formats("strict_date_time").unwrap();
        let v = serde_json::json!(1717527762025i64);
        assert!(!date_value_matches_formats(&v, &formats));
    }

    #[test]
    fn numeric_shaped_format_accepts_a_matching_number_not_just_epoch() {
        // Real ES's DateFieldMapper stringifies a JSON number before
        // handing it to the declared formatter, so a number is valid input
        // for ANY format whose textual form it matches — not just
        // epoch_millis/epoch_second. `basic_date` is `yyyyMMdd`, so the
        // number 20240101 must be accepted (previously silently dropped:
        // gating numeric input on epoch formats specifically rejected it).
        let formats = compile_formats("basic_date").unwrap();
        assert!(date_value_matches_formats(
            &serde_json::json!(20240101),
            &formats
        ));
    }

    #[test]
    fn non_matching_number_under_numeric_shaped_format_is_still_rejected() {
        let formats = compile_formats("basic_date").unwrap();
        // Not 8 digits — doesn't fit yyyyMMdd.
        assert!(!date_value_matches_formats(
            &serde_json::json!(123),
            &formats
        ));
    }

    #[test]
    fn only_genuinely_invalid_formats_fail_closed() {
        // Fail-closed is right, but it must apply to formats ES itself
        // rejects — NOT to real ES named formats. `ordinal_date` used to
        // land in the unresolvable bucket (it isn't a java.time pattern:
        // 'o' is not a pattern letter), which silently dropped every value
        // of any field declaring it. It must resolve now.
        for name in ES_NAMED_FORMATS {
            assert!(
                compile_formats(name).is_ok(),
                "real ES named format `{name}` does not resolve — every value \
                 of a field declaring it would be dropped under \
                 ignore_malformed"
            );
        }
        // A format ES also rejects (`Unknown pattern letter: b`) still fails
        // closed.
        assert!(compile_formats("banana").is_err());
        assert!(compile_formats("strict_date_time||banana").is_err());
    }

    #[test]
    fn whitespace_padded_value_is_not_trimmed() {
        // ES hands the value to the formatter as-is; a value that would
        // only match after trimming must be rejected, matching ES.
        let formats = compile_formats("strict_date_time").unwrap();
        let v = serde_json::json!(" 2024-01-01T00:00:00.000Z ");
        assert!(!date_value_matches_formats(&v, &formats));
    }

    #[test]
    fn epoch_millis_format_accepts_number_and_numeric_string() {
        let formats = compile_formats("epoch_millis").unwrap();
        assert!(date_value_matches_formats(
            &serde_json::json!(1717527762025i64),
            &formats
        ));
        assert!(date_value_matches_formats(
            &serde_json::json!("1717527762025"),
            &formats
        ));
    }

    #[test]
    fn combined_format_accepts_either_half() {
        let formats = compile_formats("strict_date_time||epoch_millis").unwrap();
        assert!(date_value_matches_formats(
            &serde_json::json!("2024-01-01T00:00:00.000Z"),
            &formats
        ));
        assert!(date_value_matches_formats(
            &serde_json::json!(1717527762025i64),
            &formats
        ));
    }

    #[test]
    fn named_strict_format_rejects_malformed_string() {
        let formats = compile_formats("strict_date_time").unwrap();
        let v = serde_json::json!("not-a-date");
        assert!(!date_value_matches_formats(&v, &formats));
    }

    #[test]
    fn null_value_always_matches() {
        let formats = compile_formats("strict_date_time").unwrap();
        assert!(date_value_matches_formats(
            &serde_json::Value::Null,
            &formats
        ));
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

// ─────────────────────────────────────────────────────────────────────────────
// Named-format truth table
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod named_format_truth_table {
    //! One row per Elasticsearch named date format: a string ES accepts, a
    //! JSON number, and whether ES accepts that number.
    //!
    //! This table exists because of a specific data-loss regression. An
    //! earlier fix made unresolvable formats fail closed — right instinct —
    //! but 28 REAL ES formats were unresolvable, because they are not
    //! java.time patterns (`basic_time` trips on `b`, `ordinal_date` on `o`,
    //! `time` on `t`) and nothing mapped them to their real pattern. Under
    //! `ignore_malformed: true` that silently dropped every value of any
    //! field declaring one of them. The formats below are now implemented,
    //! so they validate instead of vanishing.
    //!
    //! The `number_ok` column is the load-bearing one. ES's `DateFieldMapper`
    //! stringifies a JSON number and feeds it to the declared formatter, so a
    //! number is accepted exactly when its digits match the format's textual
    //! shape — true for the all-digit formats (`yyyyDDD`, `HH`, `xxxx`, …),
    //! false as soon as the format requires a separator, a `T`, or a zone
    //! offset.

    use super::*;

    struct Row {
        format: &'static str,
        /// A value real Elasticsearch accepts under `format`.
        good: &'static str,
        /// A JSON number offered under `format`.
        number: i64,
        /// Does ES accept that number? (i.e. do its digits match the
        /// format's textual shape)
        number_ok: bool,
    }

    const fn row(format: &'static str, good: &'static str, number: i64, number_ok: bool) -> Row {
        Row {
            format,
            good,
            number,
            number_ok,
        }
    }

    /// Every named format, with a real value for each.
    const TRUTH_TABLE: &[Row] = &[
        // ── epoch ────────────────────────────────────────────────────────
        row("epoch_millis", "1717527762025", 1717527762025, true),
        row("epoch_second", "1717527762", 1717527762, true),
        // ── the default, and its aliases ─────────────────────────────────
        row(
            "strict_date_optional_time",
            "2024-01-01T00:00:00.000Z",
            1717527762025,
            false,
        ),
        row("date_optional_time", "2024-01-01", 1717527762025, false),
        row("iso8601", "2024-01-01T12:10:30Z", 1717527762025, false),
        // ── basic (separator-free) ───────────────────────────────────────
        row("basic_date", "20240101", 20240101, true),
        row("basic_date_time", "20240101T121030.123Z", 20240101, false),
        row(
            "basic_date_time_no_millis",
            "20240101T121030Z",
            20240101,
            false,
        ),
        // NEWLY SUPPORTED from here down — each of these used to fail to
        // compile, which under ignore_malformed dropped every value.
        row("basic_ordinal_date", "2024001", 2024001, true),
        row(
            "basic_ordinal_date_time",
            "2024001T121030.123Z",
            2024001,
            false,
        ),
        row(
            "basic_ordinal_date_time_no_millis",
            "2024001T121030Z",
            2024001,
            false,
        ),
        row("basic_time", "121030.123Z", 121030, false),
        row("basic_time_no_millis", "121030Z", 121030, false),
        row("basic_t_time", "T121030.123Z", 121030, false),
        row("basic_t_time_no_millis", "T121030Z", 121030, false),
        row("basic_week_date", "2024W011", 2024011, false),
        row(
            "basic_week_date_time",
            "2024W011T121030.123Z",
            2024011,
            false,
        ),
        row(
            "basic_week_date_time_no_millis",
            "2024W011T121030Z",
            2024011,
            false,
        ),
        // ── calendar date / date-time ────────────────────────────────────
        row("date", "2024-01-01", 20240101, false),
        row("year_month_day", "2024-01-01", 20240101, false),
        row("date_hour", "2024-01-01T12", 20240101, false),
        row("date_hour_minute", "2024-01-01T12:10", 20240101, false),
        row(
            "date_hour_minute_second",
            "2024-01-01T12:10:30",
            20240101,
            false,
        ),
        row(
            "date_hour_minute_second_millis",
            "2024-01-01T12:10:30.123",
            20240101,
            false,
        ),
        row(
            "date_hour_minute_second_fraction",
            "2024-01-01T12:10:30.123",
            20240101,
            false,
        ),
        row(
            "date_time",
            "2024-01-01T12:10:30.123Z",
            1717527762025,
            false,
        ),
        row(
            "date_time_no_millis",
            "2024-01-01T12:10:30Z",
            1717527762025,
            false,
        ),
        row("year", "2024", 2024, true),
        row("year_month", "2024-01", 202401, false),
        // ── time of day ──────────────────────────────────────────────────
        row("hour", "12", 12, true),
        row("hour_minute", "12:10", 1210, false),
        row("hour_minute_second", "12:10:30", 121030, false),
        row("hour_minute_second_millis", "12:10:30.123", 121030, false),
        row("hour_minute_second_fraction", "12:10:30.123", 121030, false),
        row("time", "12:10:30.123Z", 121030, false),
        row("time_no_millis", "12:10:30Z", 121030, false),
        row("t_time", "T12:10:30.123Z", 121030, false),
        row("t_time_no_millis", "T12:10:30Z", 121030, false),
        // ── ordinal date ─────────────────────────────────────────────────
        row("ordinal_date", "2024-001", 2024001, false),
        row(
            "ordinal_date_time",
            "2024-001T12:10:30.123Z",
            2024001,
            false,
        ),
        row(
            "ordinal_date_time_no_millis",
            "2024-001T12:10:30Z",
            2024001,
            false,
        ),
        // ── ISO week date ────────────────────────────────────────────────
        row("week_date", "2024-W01-1", 2024011, false),
        row("week_date_time", "2024-W01-1T12:10:30.123Z", 2024011, false),
        row(
            "week_date_time_no_millis",
            "2024-W01-1T12:10:30Z",
            2024011,
            false,
        ),
        row("weekyear", "2024", 2024, true),
        row("weekyear_week", "2024-W01", 202401, false),
        row("weekyear_week_day", "2024-W01-1", 2024011, false),
    ];

    /// Values no format in the table may accept.
    const GARBAGE: &[&str] = &[
        "not-a-date",
        "",
        "banana",
        "2024-99-99T99:99:99.999Z",
        "   ",
        "null",
        "2024-01-01T00:00:00.000Z extra",
    ];

    fn accepts(format: &str, value: &serde_json::Value) -> bool {
        date_value_valid_with_format(value, format)
    }

    #[test]
    fn every_named_format_resolves() {
        // THE regression check. A format that does not compile is treated
        // as "cannot validate", which under ignore_malformed: true drops
        // every value of the field. Any name here that stops resolving is
        // silent data loss, so this asserts the whole shipped ES set — not
        // just the ones the table happens to exercise.
        for name in ES_NAMED_FORMATS {
            assert!(
                compile_formats(name).is_ok(),
                "ES named format `{name}` does not resolve: every value of a \
                 field declaring it would be silently dropped"
            );
        }
    }

    #[test]
    fn strings_elasticsearch_accepts_are_accepted() {
        for r in TRUTH_TABLE {
            assert!(
                accepts(r.format, &serde_json::json!(r.good)),
                "format `{}` rejected `{}`, which real ES accepts",
                r.format,
                r.good
            );
        }
    }

    #[test]
    fn numbers_match_elasticsearch_stringify_then_parse() {
        for r in TRUTH_TABLE {
            let got = accepts(r.format, &serde_json::json!(r.number));
            assert_eq!(
                got, r.number_ok,
                "format `{}` with the number {}: expected accept={}, got {}",
                r.format, r.number, r.number_ok, got
            );
        }
    }

    #[test]
    fn genuine_garbage_is_rejected_under_every_format() {
        for r in TRUTH_TABLE {
            for g in GARBAGE {
                assert!(
                    !accepts(r.format, &serde_json::json!(g)),
                    "format `{}` accepted garbage `{}`",
                    r.format,
                    g
                );
            }
        }
    }

    #[test]
    fn a_format_elasticsearch_itself_rejects_still_fails_closed() {
        // Fail-closed is preserved for what it was meant for: a format
        // string that is not a named format AND not a valid java.time
        // pattern. ES rejects these at mapping-creation time
        // (`Unknown pattern letter: b`), so no document can legitimately
        // arrive under one.
        assert!(compile_formats("banana").is_err());
        for g in GARBAGE {
            assert!(!accepts("banana", &serde_json::json!(g)));
        }
        assert!(!accepts("banana", &serde_json::json!(1717527762025i64)));
        assert!(!accepts("banana", &serde_json::json!("2024-01-01")));
    }

    #[test]
    fn every_newly_supported_format_would_have_failed_closed_before() {
        // Pins the exact scope of the regression that was fixed: these are
        // the names that are NOT java.time patterns, so before they were
        // added to the builtin table they compiled as literal patterns and
        // errored on their first invalid pattern letter. Each must now both
        // resolve AND validate its canonical value.
        for r in TRUTH_TABLE {
            let is_builtin_only = builtin_format_pattern(r.format).is_some()
                && compile_pattern_in(r.format, PatternDialect::User).is_err();
            if !is_builtin_only {
                continue;
            }
            assert!(
                accepts(r.format, &serde_json::json!(r.good)),
                "`{}` resolves only via the builtin table and must validate \
                 its canonical value `{}`",
                r.format,
                r.good
            );
        }
    }

    #[test]
    fn combined_formats_accept_either_side() {
        // The shape real mappings use: a named format OR epoch millis.
        for r in TRUTH_TABLE {
            let combined = format!("{}||epoch_millis", r.format);
            assert!(
                accepts(&combined, &serde_json::json!(r.good)),
                "`{combined}` rejected `{}`",
                r.good
            );
            assert!(
                accepts(&combined, &serde_json::json!(1717527762025i64)),
                "`{combined}` rejected an epoch-millis number"
            );
        }
    }

    #[test]
    fn null_is_never_malformed() {
        for r in TRUTH_TABLE {
            assert!(accepts(r.format, &serde_json::Value::Null));
        }
    }

    #[test]
    fn week_and_ordinal_dates_resolve_to_the_right_instant() {
        // Resolving is not the same as resolving CORRECTLY: check the two
        // new calendar systems land on the day they name.
        // 2024-001 is 1 Jan 2024; ISO week 2024-W01-1 is Mon 1 Jan 2024.
        let jan1 = resolve_date_bound_str("2024-01-01", false, None)
            .unwrap()
            .unwrap();
        let ordinal_fmt = compile_formats("ordinal_date").unwrap();
        let week_fmt = compile_formats("week_date").unwrap();
        let ordinal = resolve_date_bound_str("2024-001", false, Some(&ordinal_fmt))
            .unwrap()
            .unwrap();
        let week = resolve_date_bound_str("2024-W01-1", false, Some(&week_fmt))
            .unwrap()
            .unwrap();
        assert_eq!(ordinal, jan1, "ordinal_date 2024-001 should be 2024-01-01");
        assert_eq!(week, jan1, "week_date 2024-W01-1 should be 2024-01-01");

        // 2024 is a leap year: day 366 exists and is 31 Dec.
        let dec31 = resolve_date_bound_str("2024-12-31", false, None)
            .unwrap()
            .unwrap();
        assert_eq!(
            resolve_date_bound_str("2024-366", false, Some(&ordinal_fmt)),
            Ok(Some(dec31))
        );
        // 2023 is not: day 366 must not resolve.
        assert!(!accepts("ordinal_date", &serde_json::json!("2023-366")));
        // Week 53 does not exist in 2024 (a 52-week ISO year).
        assert!(!accepts("week_date", &serde_json::json!("2024-W53-1")));
        // ...but it does in 2020.
        assert!(accepts("week_date", &serde_json::json!("2020-W53-1")));
    }

    #[test]
    fn compiled_formats_are_cached_and_the_cache_is_transparent() {
        // The cache must not change any answer, only avoid the recompile.
        for r in TRUTH_TABLE {
            let uncached = compile_formats(r.format)
                .map(|f| date_value_matches_formats(&serde_json::json!(r.good), &f))
                .unwrap_or(false);
            assert_eq!(
                uncached,
                accepts(r.format, &serde_json::json!(r.good)),
                "cached and uncached disagree for `{}`",
                r.format
            );
        }
        // Same string twice hands back the same compiled Arc.
        let a = compile_formats_cached("strict_date_time||epoch_millis").unwrap();
        let b = compile_formats_cached("strict_date_time||epoch_millis").unwrap();
        assert!(
            Arc::ptr_eq(&a, &b),
            "second compile of an identical format string should be a cache hit"
        );
    }
}

#[cfg(test)]
mod locale_dependent_text_fields {
    //! A pattern with a textual month/weekday is locale-dependent, and this
    //! engine only knows English month names.  ES honours the mapping's
    //! `locale`, so rejecting a French month name would drop documents real
    //! ES indexes — and it would do so on BOTH ingest paths at once (dropped
    //! under `ignore_malformed: true`, whole document rejected under
    //! `false`).  The structure around the word is still validated.

    use super::*;

    /// The mapping from the `180_locale_dependent_mapping` conformance test.
    const FR_FORMAT: &str = "E, d MMM yyyy HH:mm:ss Z";

    fn accepts(format: &str, value: &str) -> bool {
        date_value_valid_with_format(&serde_json::json!(value), format)
    }

    #[test]
    fn french_month_names_are_accepted_not_dropped() {
        assert!(accepts(FR_FORMAT, "mer., 6 déc. 2000 02:55:00 -0800"));
        assert!(accepts(FR_FORMAT, "jeu., 7 déc. 2000 02:55:00 -0800"));
    }

    #[test]
    fn english_month_names_still_take_the_strict_path() {
        assert!(accepts(FR_FORMAT, "Wed, 6 Dec 2000 02:55:00 -0800"));
    }

    #[test]
    fn locale_tolerance_only_covers_the_word_not_the_structure() {
        // Everything except which language the month is in is still checked.
        for bad in [
            "not a date at all",
            "mer., 6 déc. 2000 02:55:00",        // missing the offset
            "mer., 6 déc. 2000 99:99:99 -0800",  // impossible time
            "mer., 6 déc. 20 02:55:00 -0800",    // 2-digit year
            "mer., déc. 2000 02:55:00 -0800",    // missing the day
            ", 6 déc. 2000 02:55:00 -0800",      // missing the weekday
            "mer., 6 déc. 2000 02:55:00 -0800!", // trailing garbage
        ] {
            assert!(
                !accepts(FR_FORMAT, bad),
                "locale tolerance wrongly accepted `{bad}`"
            );
        }
    }

    #[test]
    fn locale_tolerance_does_not_leak_into_range_bound_resolution() {
        // Bound resolution needs a real month number, so it stays English-
        // only: a French month resolves to nothing rather than silently
        // becoming January.
        let fmt = compile_formats(FR_FORMAT).unwrap();
        assert_eq!(
            resolve_date_bound_str("mer., 6 déc. 2000 02:55:00 -0800", false, Some(&fmt)),
            Err(DateResolveError::UnparseableValue(
                "mer., 6 déc. 2000 02:55:00 -0800".to_string()
            ))
        );
        // The English spelling of the same instant does resolve.
        assert!(matches!(
            resolve_date_bound_str("Wed, 6 Dec 2000 02:55:00 -0800", false, Some(&fmt)),
            Ok(Some(_))
        ));
    }

    #[test]
    fn a_format_without_text_fields_gets_no_tolerance() {
        // The carve-out is scoped to patterns that actually have a textual
        // month/weekday — nothing else becomes laxer.
        assert!(!accepts("yyyy-MM-dd", "banana"));
        assert!(!accepts("strict_date_time", "mer., 6 déc. 2000"));
    }
}

#[cfg(test)]
mod strict_and_lenient_agree {
    //! `ignore_malformed: false` (reject the document) and
    //! `ignore_malformed: true` (drop the field) must make the SAME
    //! malformed/not-malformed call.  They didn't: the strict path had its
    //! own implementation that ended in a blanket `true` for named and
    //! custom patterns, so asking for stricter handling made the engine
    //! accept strictly MORE.  Both now call
    //! [`date_value_valid_with_format`]; this pins the values that used to
    //! show the contradiction.

    use super::*;

    /// Values the old strict path waved through under a declared format.
    const WAS_WRONGLY_ACCEPTED_WHEN_STRICT: &[(&str, &str)] = &[
        ("dd/MM/yyyy", "literally anything"),
        ("dd/MM/yyyy", "2024-01-01"),
        ("strict_date_time", "not-a-date"),
        ("basic_date", "nonsense"),
        ("yyyy-MM-dd HH:mm:ss", "whenever"),
        ("ordinal_date", "banana"),
    ];

    #[test]
    fn strict_no_longer_accepts_what_lenient_rejects() {
        for (fmt, value) in WAS_WRONGLY_ACCEPTED_WHEN_STRICT {
            assert!(
                !date_value_valid_with_format(&serde_json::json!(value), fmt),
                "`{value}` under `{fmt}` is malformed and must be rejected by \
                 both ingest paths"
            );
        }
    }

    #[test]
    fn the_two_paths_share_one_predicate() {
        // Whatever the answer is, it is the same answer — the property that
        // was violated. Exercised across the whole named-format set with a
        // spread of value shapes.
        for name in ES_NAMED_FORMATS {
            for v in [
                serde_json::json!("2024-01-01T00:00:00.000Z"),
                serde_json::json!("garbage"),
                serde_json::json!(1717527762025i64),
                serde_json::json!(2024),
                serde_json::Value::Null,
            ] {
                let a = date_value_valid_with_format(&v, name);
                let b = compile_formats(name)
                    .map(|f| date_value_matches_formats(&v, &f))
                    .unwrap_or(false);
                assert_eq!(a, b, "format `{name}` disagreed on {v:?}");
            }
        }
    }
}

#[cfg(test)]
mod conformance_corpus_values {
    //! Every `(declared format, indexed value)` pair that appears in the
    //! ES-compat YAML corpus under `engine/tests/es-compat-yaml/yaml`.
    //!
    //! These matter more than they look. The `ignore_malformed: false` path
    //! used to accept every string under a named or custom pattern, so it
    //! never rejected any of these. Now that both ingest paths share one real
    //! parser, a value here that fails to parse stops being indexed and
    //! starts being a `document_parsing_exception` — a conformance failure
    //! and, in production, a rejected document. Two were found this way:
    //! `strict_date_time` was requiring the `.SSS` that ES makes optional,
    //! and the French `MMM` month names had no locale tolerance.

    use super::*;

    const CORPUS: &[(&str, &str)] = &[
        // aggregations/range.yml — the pair that exposed the mandatory-.SSS
        // bug: real ES accepts these under strict_date_time with no millis.
        ("strict_date_time||strict_date", "2021-05-01T07:10:00Z"),
        ("strict_date_time||strict_date", "2021-05-02T08:34:00Z"),
        ("strict_date_time||strict_date", "2021-05-03T08:36:00Z"),
        ("strict_date_time||strict_date", "2021-05-04T09:05:00Z"),
        ("strict_date_time||strict_date", "2021-05-06T09:22:00Z"),
        ("strict_date_time||strict_date", "2015-01-01"),
        // aggregations/composite.yml, date_histogram.yml,
        // ignored_metadata_field.yml
        ("yyyy-MM-dd HH:mm:ss", "2021-05-01 20:00:00"),
        ("yyyy-MM-dd HH:mm:ss", "2021-05-01 21:20:00"),
        ("yyyy-MM-dd HH:mm:ss", "2021-05-01 23:54:00"),
        // aggregations/date_range.yml — epoch_second, given as strings.
        ("epoch_second", "28800000000"),
        ("epoch_second", "315561600000"),
        ("epoch_second", "631180800000"),
        // aggregations/range_timezone_bug.yml — 9-digit fraction + offset.
        (
            "uuuu-MM-dd'T'HH:mm:ss.SSSSSSSSSZZZZZ",
            "2021-08-12T01:00:00.000000000+02:00",
        ),
        // search/500_date_range.yml — bare `uuuu` years, including 5 digits.
        ("uuuu", "1900"),
        ("uuuu", "2022"),
        ("uuuu", "1500"),
        ("uuuu", "10000"),
        // search/390_doc_values_search.yml
        ("yyyy/MM/dd", "2017/01/01"),
        ("yyyy/MM/dd", "2017/01/02"),
        // search/530_ignore_above_stored_source.yml
        ("yyyy-MM-dd'T'HH:mm:ss", "2017-10-20T03:08:45"),
        ("yyyy-MM-dd'T'HH:mm:ss", "2017-10-21T07:00:00"),
        ("yyyy-MM-dd'T'HH:mm:ss", "2017-10-22T01:00:00"),
        // search/90_search_after.yml, search/630_format_sort_missing_dates.yml
        ("yyyy-MM-dd HH:mm:ss.SSS", "2019-10-21 00:30:04.828"),
        ("yyyy-MM-dd HH:mm:ss.SSS", "2021-02-11 08:30:04.828"),
        ("yyyy-MM-dd HH:mm:ss.SSS", "2021-10-13 00:30:04.828"),
        ("yyyy-MM-dd HH:mm:ss.SSSSSS", "2019-10-21 00:30:04.828740"),
        ("yyyy-MM-dd HH:mm:ss.SSSSSS", "2021-06-11 04:30:04.828456"),
        ("yyyy-MM-dd HH:mm:ss.SSSSSS", "2021-10-13 00:30:04.828123"),
        ("dd/MM/yyyy HH:mm:ss.SSS", "15/04/2021 06:30:04.821"),
        ("dd/MM/yyyy HH:mm:ss.SSS", "20/05/2021 05:30:04.832"),
        ("dd/MM/yyyy HH:mm:ss.SSS", "21/08/2021 03:30:04.732"),
        // search/180_locale_dependent_mapping.yml — locale: fr
        (
            "E, d MMM yyyy HH:mm:ss Z",
            "mer., 6 déc. 2000 02:55:00 -0800",
        ),
        (
            "E, d MMM yyyy HH:mm:ss Z",
            "jeu., 7 déc. 2000 02:55:00 -0800",
        ),
        // search/140_pre_filter_search_shards.yml
        ("yyyy-MM-dd", "2015-01-01"),
        ("yyyy-MM-dd", "2016-02-01"),
    ];

    #[test]
    fn no_corpus_value_is_rejected() {
        for (fmt, value) in CORPUS {
            assert!(
                date_value_valid_with_format(&serde_json::json!(value), fmt),
                "`{value}` under declared format `{fmt}` would now be dropped \
                 (ignore_malformed: true) or rejected outright \
                 (ignore_malformed: false) — real ES indexes it"
            );
        }
    }

    #[test]
    fn strict_date_time_millis_are_optional_both_ways() {
        // ES builds strict_date_time's fraction with optionalStart(), so
        // both spellings are valid; requiring `.SSS` rejected real data.
        for v in [
            "2021-05-01T07:10:00Z",
            "2021-05-01T07:10:00.123Z",
            "2021-05-01T07:10:00+02:00",
            "2021-05-01T07:10:00.123+02:00",
        ] {
            assert!(
                date_value_valid_with_format(&serde_json::json!(v), "strict_date_time"),
                "strict_date_time rejected `{v}`"
            );
        }
        // The offset is still required, and the seconds still are too.
        assert!(!date_value_valid_with_format(
            &serde_json::json!("2021-05-01T07:10:00"),
            "strict_date_time"
        ));
        assert!(!date_value_valid_with_format(
            &serde_json::json!("2021-05-01T07:10Z"),
            "strict_date_time"
        ));
    }

    #[test]
    fn optional_sections_are_builtin_only() {
        // `[` keeps its old literal meaning in a user-supplied pattern.
        let toks = compile_pattern_in("yyyy[MM", PatternDialect::User).unwrap();
        assert!(toks
            .iter()
            .any(|t| matches!(t, PatTok::Literal(l) if l == "[")));
        assert!(date_value_valid_with_format(
            &serde_json::json!("2024[01"),
            "yyyy[MM"
        ));
    }
}

#[cfg(test)]
mod ignored_metadata_field_oracle {
    //! `aggregations/ignored_metadata_field.yml` is an ES fixture that
    //! asserts an exact `_ignored` count, so it states, per value, whether
    //! real Elasticsearch considers it malformed. That makes it a ready-made
    //! truth table for the `ignore_malformed: true` path — the one the
    //! regression under repair silently emptied.

    use super::*;

    fn valid(fmt: &str, v: &str) -> bool {
        date_value_valid_with_format(&serde_json::json!(v), fmt)
    }

    /// `date_of_birth`, declared `format: "dd-MM-yyyy"`.
    #[test]
    fn date_of_birth_dd_mm_yyyy() {
        for good in [
            "12-03-1990",
            "15-05-1991",
            "01-09-1994",
            "05-06-1989",
            "16-11-1990",
            "18-12-1992",
        ] {
            assert!(valid("dd-MM-yyyy", good), "`{good}` should be indexed");
        }
        for bad in [
            "12-03-1990 12:30:45", // trailing time
            "19-12-90",            // 2-digit year
            "20/03/1992",          // wrong separator
            "02311988",            // no separators
            "17.15.1990",          // wrong separator + month 15
        ] {
            assert!(!valid("dd-MM-yyyy", bad), "`{bad}` should be _ignored");
        }
    }

    /// `order_datetime`, declared `format: "yyyy-MM-dd HH:mm:ss"`.
    #[test]
    fn order_datetime_yyyy_mm_dd_hh_mm_ss() {
        for good in [
            "2021-05-01 20:01:37",
            "2021-05-03 19:38:22",
            "2021-05-01 20:05:37",
        ] {
            assert!(
                valid("yyyy-MM-dd HH:mm:ss", good),
                "`{good}` should be indexed"
            );
        }
        for bad in [
            "2021-05-02",              // date only
            "2021-05-01-20:01:37",     // '-' where ' ' belongs
            "20210501 20:01:37",       // no date separators
            "2021-05-01 20:01:37.123", // trailing millis
            "2021-05-03 20:01",        // no seconds
            "2021-05-03 20-01-55",     // '-' time separators
            "2021-05-01T20:02:00",     // 'T' where ' ' belongs
        ] {
            assert!(
                !valid("yyyy-MM-dd HH:mm:ss", bad),
                "`{bad}` should be _ignored"
            );
        }
    }
}
