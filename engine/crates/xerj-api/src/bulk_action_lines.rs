//! Elasticsearch's whole-request check of `_bulk` action lines.
//!
//! ES parses every action line of a bulk body before it executes any action
//! (`BulkRequestParser#parse`, 8.13). A line it cannot read as an action
//! refuses the whole request with a 400 `illegal_argument_exception` that
//! names the line, so nothing in the body is written. The bulk engine reads
//! action lines on its parallel parse and reports a bad one as a per-item
//! error after the rest have run; this check runs first, on the HTTP path,
//! and gives ES's answer instead.
//!
//! It is a byte scanner, not a JSON parse: action lines are short, and the
//! bulk hot path deliberately avoids building a `Value` per action (M5.12 in
//! `xerj_engine::bulk`). It reads names and token shapes the way ES's
//! streaming parser meets them, and refuses only what it reads with
//! confidence. Anything else (JSON it cannot follow, an escaped key, a byte
//! order mark) stops the check at that line and leaves the rest of the body
//! to the engine, which answers exactly as before. Stopping, rather than
//! skipping the line, matters: once a line is unread, which line is an action
//! and which is a source is no longer certain, and reading a source line as
//! an action would refuse a body ES accepts.
//!
//! It does not convert values. ES does (`version` to a number, `_source` to a
//! fetch context), so for a line that also has a value ES cannot convert, ES
//! can name that value where this check names a later problem. Both answers
//! are a 400 for the whole request.

/// Fields ES reads as a simple value inside an action. `_type` is absent on
/// purpose: on the 8.x REST API it is an unknown parameter (see
/// `first_malformed_action_line` for the 7.x compatibility case).
const SIMPLE_FIELDS: &[&[u8]] = &[
    b"_index",
    b"_id",
    b"routing",
    b"op_type",
    b"version",
    b"version_type",
    b"if_seq_no",
    b"if_primary_term",
    b"retry_on_conflict",
    b"pipeline",
    b"_source",
    b"require_alias",
    b"require_data_stream",
    b"list_executed_pipelines",
];

/// Fields ES accepts as an object inside an action.
const OBJECT_FIELDS: &[&[u8]] = &[b"dynamic_templates", b"_source"];

/// The reason ES gives for the first malformed action line in `body`, or
/// `None` when every action line is one ES would accept, or the check
/// stopped at a line it leaves to the engine. Lines are numbered from 1 and
/// blank lines count, as in ES. Pairing follows the bulk engine: every action
/// but a delete is followed by one source line.
///
/// `compat_v7` is a request sent with ES's REST compatibility header
/// (`compatible-with=7`); ES 8 then accepts `_type` with a deprecation
/// warning instead of refusing it.
pub(crate) fn first_malformed_action_line(body: &str, compat_v7: bool) -> Option<String> {
    let mut expect_source = false;
    for (n, line) in body.split('\n').enumerate() {
        if line.trim().is_empty() {
            if expect_source {
                // ES takes the very next line as the source even when it is
                // blank; the engine skips blank lines. The two pair every
                // line after this one differently, so stop here.
                return None;
            }
            continue;
        }
        if expect_source {
            expect_source = false;
            continue;
        }
        match check(line.as_bytes(), n + 1, compat_v7) {
            Line::Refused(reason) => return Some(reason),
            Line::Deferred => return None,
            Line::Accepted => {}
        }
        expect_source = !crate::cluster_shard::bulk_action_line_is_delete(line);
    }
    None
}

enum Line {
    Accepted,
    Refused(String),
    /// Not something this scanner reads with confidence; the engine decides.
    Deferred,
}

/// A JSON number, the whole of `t`.
fn is_json_number(t: &[u8]) -> bool {
    let digits = |i: &mut usize| {
        let start = *i;
        while matches!(t.get(*i), Some(b'0'..=b'9')) {
            *i += 1;
        }
        *i > start
    };
    let mut i = 0;
    if t.first() == Some(&b'-') {
        i += 1;
    }
    match t.get(i) {
        Some(b'0') => i += 1,
        Some(b'1'..=b'9') => {
            digits(&mut i);
        }
        _ => return false,
    }
    if t.get(i) == Some(&b'.') {
        i += 1;
        if !digits(&mut i) {
            return false;
        }
    }
    if matches!(t.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(t.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        if !digits(&mut i) {
            return false;
        }
    }
    i == t.len()
}

struct Cursor<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Cursor<'a> {
    fn peek(&mut self) -> Option<u8> {
        while self.p < self.b.len() && matches!(self.b[self.p], b' ' | b'\t' | b'\r' | b'\n') {
            self.p += 1;
        }
        self.b.get(self.p).copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.p += 1;
            true
        } else {
            false
        }
    }

    /// A key with no escapes, consumed with its closing quote. `None` for an
    /// escaped or unterminated key.
    fn key(&mut self) -> Option<&'a [u8]> {
        if !self.eat(b'"') {
            return None;
        }
        let start = self.p;
        while self.p < self.b.len() {
            match self.b[self.p] {
                b'"' => {
                    self.p += 1;
                    return Some(&self.b[start..self.p - 1]);
                }
                b'\\' => return None,
                _ => self.p += 1,
            }
        }
        None
    }

    /// Consume a string, number or literal and name its token as ES's parser
    /// does. `None` when it is not a complete JSON scalar: the JSON parser in
    /// ES would fail on it before ES looks at the line, with another error.
    fn scalar(&mut self) -> Option<&'static str> {
        if self.peek()? == b'"' {
            self.p += 1;
            while self.p < self.b.len() {
                match self.b[self.p] {
                    b'\\' => self.p += 2,
                    b'"' => {
                        self.p += 1;
                        return Some("VALUE_STRING");
                    }
                    _ => self.p += 1,
                }
            }
            return None;
        }
        let start = self.p;
        while self.p < self.b.len()
            && !matches!(
                self.b[self.p],
                b',' | b'}' | b']' | b' ' | b'\t' | b'\r' | b'\n'
            )
        {
            self.p += 1;
        }
        match &self.b[start..self.p] {
            b"true" | b"false" => Some("VALUE_BOOLEAN"),
            b"null" => Some("VALUE_NULL"),
            t if is_json_number(t) => Some("VALUE_NUMBER"),
            _ => None,
        }
    }

    /// Skip a balanced object starting at `{`. `false` when it does not end.
    fn skip_object(&mut self) -> bool {
        let mut depth = 0usize;
        let mut in_str = false;
        while self.p < self.b.len() {
            let c = self.b[self.p];
            self.p += 1;
            if in_str {
                match c {
                    b'\\' => self.p += 1,
                    b'"' => in_str = false,
                    _ => {}
                }
                continue;
            }
            match c {
                b'"' => in_str = true,
                b'{' | b'[' => depth += 1,
                b'}' | b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return true;
                    }
                }
                _ => {}
            }
        }
        false
    }
}

fn check(line: &[u8], n: usize, compat_v7: bool) -> Line {
    let malformed =
        |what: &str| Line::Refused(format!("Malformed action/metadata line [{n}], {what}"));
    let mut c = Cursor { b: line, p: 0 };

    match c.peek() {
        Some(b'{') => c.p += 1,
        Some(b'[') => return malformed("expected START_OBJECT but found [START_ARRAY]"),
        Some(_) => {
            return match c.scalar() {
                Some(t) => malformed(&format!("expected START_OBJECT but found [{t}]")),
                None => Line::Deferred,
            }
        }
        None => return Line::Deferred,
    }

    match c.peek() {
        Some(b'"') => {}
        Some(b'}') => return malformed("expected FIELD_NAME but found [END_OBJECT]"),
        _ => return Line::Deferred,
    }
    let Some(action) = c.key() else {
        return Line::Deferred;
    };
    if !c.eat(b':') {
        return Line::Deferred;
    }
    // ES's JSON parser reads the start of a value together with its name, so
    // a value it cannot read fails before the name is looked at.
    let value = match c.peek() {
        Some(b'{') => {
            c.p += 1;
            None
        }
        Some(b'[') => Some("START_ARRAY"),
        Some(_) => match c.scalar() {
            Some(t) => Some(t),
            None => return Line::Deferred,
        },
        None => return Line::Deferred,
    };
    if !matches!(action, b"create" | b"delete" | b"index" | b"update") {
        return malformed(&format!(
            "expected field [create], [delete], [index] or [update] but found [{}]",
            String::from_utf8_lossy(action)
        ));
    }
    if let Some(t) = value {
        return malformed(&format!(
            "expected START_OBJECT or END_OBJECT but found [{t}]"
        ));
    }

    if c.eat(b'}') {
        return Line::Accepted;
    }
    loop {
        let Some(field) = c.key() else {
            return Line::Deferred;
        };
        if !c.eat(b':') {
            return Line::Deferred;
        }
        let name = || String::from_utf8_lossy(field);
        match c.peek() {
            Some(b'[') => {
                return malformed(&format!(
                    "expected a simple value for field [{}] but found [START_ARRAY]",
                    name()
                ))
            }
            Some(b'{') => {
                if !OBJECT_FIELDS.contains(&field) {
                    return malformed(&format!(
                        "expected a simple value for field [{}] but found [START_OBJECT]",
                        name()
                    ));
                }
                if !c.skip_object() {
                    return Line::Deferred;
                }
            }
            Some(_) => match c.scalar() {
                None => return Line::Deferred,
                // ES skips a null for any field without looking at its name.
                Some("VALUE_NULL") => {}
                Some(_) => {
                    let known = SIMPLE_FIELDS.contains(&field) || (compat_v7 && field == b"_type");
                    if !known {
                        return Line::Refused(format!(
                            "Action/metadata line [{n}] contains an unknown parameter [{}]",
                            name()
                        ));
                    }
                }
            },
            None => return Line::Deferred,
        }
        if c.eat(b',') {
            continue;
        }
        // ES stops reading at the action's closing brace; what follows it is
        // only a deprecation warning there, so it is not checked here.
        return if c.eat(b'}') {
            Line::Accepted
        } else {
            Line::Deferred
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason(body: &str) -> Option<String> {
        first_malformed_action_line(body, false)
    }

    #[test]
    fn canonical_bodies_pass() {
        assert_eq!(reason(""), None);
        assert_eq!(reason("\n\n"), None);
        assert_eq!(
            reason("{\"index\":{}}\n{\"a\":1}\n{\"delete\":{\"_id\":\"1\"}}\n{\"create\":{\"_id\":1}}\n{}\n"),
            None,
            "a source line is never read as an action, even when it is `{{}}`"
        );
        assert_eq!(
            reason("{\"index\":{\"_id\":\"a\\\"b\",\"_source\":{\"x\":[1,{\"y\":\"}\"}]}}}\n{}\n"),
            None
        );
        assert_eq!(
            reason("\r\n{ \"index\" : { \"_id\" : \"1\" } }\r\n{\"a\":1}\r\n"),
            None
        );
    }

    #[test]
    fn the_first_problem_is_the_one_reported() {
        assert_eq!(
            reason("{\"index\":{\"_type\":\"x\",\"_id\":[1]}}\n{}\n").as_deref(),
            Some("Action/metadata line [1] contains an unknown parameter [_type]")
        );
        assert_eq!(
            reason("{\"index\":{\"_id\":[1],\"_type\":\"x\"}}\n{}\n").as_deref(),
            Some(
                "Malformed action/metadata line [1], expected a simple value for field [_id] but found [START_ARRAY]"
            )
        );
    }

    #[test]
    fn a_value_es_ignores_is_ignored() {
        assert_eq!(reason("{\"index\":{\"anything\":null}}\n{}\n"), None);
        assert_eq!(
            reason("{\"index\":{\"_index\":\"t\"}} trailing\n{}\n"),
            None
        );
    }

    #[test]
    fn what_it_cannot_read_is_left_to_the_engine() {
        for line in [
            "{\"index\":",
            "{\"ind",
            "{\"\\u0069ndex\":{}}",
            "{\"index\":{\"_\\u0069d\":\"1\"}}",
            "{\"index\":{\"_id\":\"1\"",
            "{index:{}}",
            "garbage",
        ] {
            assert_eq!(reason(&format!("{line}\n{{}}\n")), None, "{line}");
        }
    }

    #[test]
    fn a_delete_takes_no_source_line() {
        assert_eq!(
            reason("{\"delete\":{\"_id\":\"1\"}}\n{\"bogus\":{}}\n").as_deref(),
            Some(
                "Malformed action/metadata line [2], expected field [create], [delete], [index] or [update] but found [bogus]"
            )
        );
    }

    #[test]
    fn every_name_es_accepts_is_accepted() {
        // Written out by hand from ES 8.13's `BulkRequestParser` fields, not
        // read from `SIMPLE_FIELDS`, so a typo there fails here.
        for (field, value) in [
            ("_index", "\"t\""),
            ("_id", "\"1\""),
            ("_id", "1"),
            ("routing", "\"r\""),
            ("op_type", "\"create\""),
            ("version", "3"),
            ("version_type", "\"external\""),
            ("if_seq_no", "0"),
            ("if_primary_term", "1"),
            ("retry_on_conflict", "3"),
            ("pipeline", "\"p\""),
            ("_source", "true"),
            ("_source", "\"a\""),
            ("_source", "{\"includes\":[\"a\"]}"),
            ("require_alias", "false"),
            ("require_data_stream", "true"),
            ("list_executed_pipelines", "false"),
            ("dynamic_templates", "{\"a\":\"b\"}"),
            ("version", "-1.5e+3"),
        ] {
            let body = format!("{{\"update\":{{\"{field}\":{value}}}}}\n{{}}\n");
            assert_eq!(reason(&body), None, "{field}: {value}");
        }
    }

    #[test]
    fn a_complete_scalar_is_named_by_its_token() {
        for (line, t) in [
            ("\"abc\"", "expected START_OBJECT but found [VALUE_STRING]"),
            ("true", "expected START_OBJECT but found [VALUE_BOOLEAN]"),
            ("12", "expected START_OBJECT but found [VALUE_NUMBER]"),
            (
                "{\"index\":null}",
                "expected START_OBJECT or END_OBJECT but found [VALUE_NULL]",
            ),
            (
                "{\"index\":false}",
                "expected START_OBJECT or END_OBJECT but found [VALUE_BOOLEAN]",
            ),
            (
                "{\"index\":-0.5}",
                "expected START_OBJECT or END_OBJECT but found [VALUE_NUMBER]",
            ),
        ] {
            assert_eq!(
                reason(&format!("{line}\n{{}}\n")),
                Some(format!("Malformed action/metadata line [1], {t}")),
                "{line}"
            );
        }
    }

    #[test]
    fn json_es_could_not_read_is_never_given_an_es_reason() {
        // Each of these fails in ES's JSON parser before ES reads the line,
        // with a parse error this check does not imitate.
        for line in [
            "nope",
            "tru",
            "1x",
            "01",
            "-",
            "}",
            "]",
            "{\"index\":}",
            "{\"index\":]",
            "{\"index\":nul}",
            "{\"bogus\"}",
            "{\"bogus\":x}",
            "{\"index\":{\"_type\":tru}}",
            "{\"index\":{\"_type\":1.}}",
            "{\"index\":{\"_id\":\"1\",}}",
            "{\"index\":{\"_id\":\"1\" \"x\":1}}",
        ] {
            assert_eq!(reason(&format!("{line}\n{{}}\n")), None, "{line}");
        }
    }

    #[test]
    fn the_check_stops_where_pairing_is_in_doubt() {
        // An escaped `delete` key: ES reads a delete (no source line), this
        // scanner cannot. Reading on would check `{"a":1}` as an action.
        assert_eq!(
            reason("{\"\\u0064elete\":{\"_index\":\"t\",\"_id\":\"1\"}}\n{\"index\":{\"_index\":\"t\"}}\n{\"a\":1}\n"),
            None
        );
        // A byte order mark before a delete, which ES's parser skips.
        assert_eq!(
            reason("\u{feff}{\"delete\":{\"_index\":\"t\",\"_id\":\"1\"}}\n{\"index\":{}}\n{\"a\":1}\n"),
            None
        );
        // A blank line where a source belongs: ES takes it as the source,
        // the engine skips it.
        assert_eq!(
            reason("{\"index\":{}}\n\n{\"a\":1}\n{\"index\":{}}\n{\"b\":1}\n"),
            None
        );
        // Before any doubt, a bad line is still found.
        assert_eq!(
            reason("{\"index\":{}}\n{\"a\":1}\n{}\n{\"\\u0064elete\":{}}\n").as_deref(),
            Some("Malformed action/metadata line [3], expected FIELD_NAME but found [END_OBJECT]")
        );
    }

    #[test]
    fn rest_compatibility_with_7_accepts_type() {
        let body = "{\"index\":{\"_index\":\"t\",\"_type\":\"_doc\",\"_id\":\"1\"}}\n{}\n";
        assert_eq!(
            reason(body).as_deref(),
            Some("Action/metadata line [1] contains an unknown parameter [_type]")
        );
        assert_eq!(first_malformed_action_line(body, true), None);
        // Only `_type` comes back, and only as a simple value.
        assert_eq!(
            first_malformed_action_line("{\"index\":{\"_version\":1}}\n{}\n", true).as_deref(),
            Some("Action/metadata line [1] contains an unknown parameter [_version]")
        );
        assert_eq!(
            first_malformed_action_line("{\"index\":{\"_type\":{}}}\n{}\n", true).as_deref(),
            Some(
                "Malformed action/metadata line [1], expected a simple value for field [_type] but found [START_OBJECT]"
            )
        );
    }
}
