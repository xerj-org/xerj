//! `template_name_matches` is ES `Regex.simpleMatch`: `*` matches any run of
//! characters, including none, and nothing else is special.

use xerj_engine::engine::{is_template_pattern, template_name_matches};

#[test]
fn star_matches_any_run_including_none() {
    assert!(template_name_matches("*", "anything"));
    assert!(template_name_matches("*", ""));
    assert!(template_name_matches("logs-*", "logs-"));
    assert!(template_name_matches("logs-*", "logs-app"));
    assert!(template_name_matches("*-b", "logs-b"));
    assert!(template_name_matches("l*s-*", "logs-a"));
    assert!(template_name_matches("a*b*c", "aXXbYYc"));
    assert!(template_name_matches("a**b", "ab"));
}

#[test]
fn everything_else_is_literal() {
    assert!(template_name_matches("logs", "logs"));
    assert!(!template_name_matches("logs", "logs-a"));
    assert!(!template_name_matches("logs-*", "metrics"));
    assert!(!template_name_matches("*-b", "logs-a"));
    assert!(!template_name_matches("a*b*c", "aXXbYY"));
    // `?` is not a wildcard in ES's simpleMatch.
    assert!(!template_name_matches("log?", "logs"));
    assert!(template_name_matches("log?", "log?"));
}

#[test]
fn only_a_star_makes_a_pattern() {
    assert!(is_template_pattern("logs-*"));
    assert!(is_template_pattern("*"));
    assert!(!is_template_pattern("logs"));
    assert!(!is_template_pattern("_all"));
    assert!(!is_template_pattern("a,b"));
}
