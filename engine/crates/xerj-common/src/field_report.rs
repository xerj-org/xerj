//! JSON body for `POST /api/field-report`.
//!
//! Shared by `xerj feedback --submit` and the MCP tool `xerj_field_report` so
//! the two cannot disagree about field names or the length caps the Pages
//! function enforces (`functions/api/field-report.js`,
//! `landing/docs/agents/schemas/field-report.json`).
//!
//! The HTTP call itself stays in the caller: this crate does not depend on
//! reqwest. A caller that posts must not attach the node's `Authorization`
//! header — that key is for the local engine, not for xerj.org.

use serde_json::{json, Value};

/// Production intake. Override with [`URL_ENV`] for a mock or a staging host.
pub const URL_DEFAULT: &str = "https://xerj.org/api/field-report";

/// Process environment that replaces [`URL_DEFAULT`]. Empty is unset.
pub const URL_ENV: &str = "XERJ_FIELD_REPORT_URL";

const CAP_AGENT: usize = 120;
const CAP_USED_FOR: usize = 1000;
/// The published schema caps `verdict` at 1000 (aligned with `used_for`).
const CAP_VERDICT: usize = 1000;
const CAP_VERSION: usize = 80;
const CAP_OS: usize = 120;
const CAP_INDEXED: usize = 200;
const CAP_NOTES: usize = 4000;

/// What the caller knows, before caps. Empty strings become empty fields.
#[derive(Debug, Clone)]
pub struct Input<'a> {
    pub agent: &'a str,
    pub used_for: &'a str,
    pub verdict: &'a str,
    pub version: &'a str,
    pub os: &'a str,
    pub indexed: &'a str,
    pub notes: &'a str,
}

/// The body that will be posted, after control-character stripping and caps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payload {
    pub agent: String,
    pub used_for: String,
    pub verdict: String,
    pub version: String,
    pub os: String,
    pub indexed: String,
    pub notes: String,
}

impl Payload {
    pub fn from_input(input: &Input<'_>) -> Self {
        Self {
            agent: cap(input.agent, CAP_AGENT),
            used_for: cap(input.used_for, CAP_USED_FOR),
            verdict: cap(input.verdict, CAP_VERDICT),
            version: cap(input.version, CAP_VERSION),
            os: cap(input.os, CAP_OS),
            indexed: cap(input.indexed, CAP_INDEXED),
            notes: cap(input.notes, CAP_NOTES),
        }
    }

    /// Required fields the endpoint would reject. Empty after capping.
    pub fn missing_required(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.agent.is_empty() {
            missing.push("agent");
        }
        if self.used_for.is_empty() {
            missing.push("used_for");
        }
        if self.verdict.is_empty() {
            missing.push("verdict");
        }
        missing
    }

    pub fn to_json(&self) -> Value {
        json!({
            "agent": self.agent,
            "used_for": self.used_for,
            "verdict": self.verdict,
            "version": self.version,
            "os": self.os,
            "indexed": self.indexed,
            "notes": self.notes,
        })
    }

    pub fn json_string(&self) -> String {
        self.to_json().to_string()
    }
}

/// `XERJ_FIELD_REPORT_URL` when it is non-empty, otherwise [`URL_DEFAULT`].
pub fn endpoint_from_env() -> String {
    match std::env::var(URL_ENV) {
        Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => URL_DEFAULT.to_string(),
    }
}

/// A copy-pasteable `curl` for `body`. Single-quoted so the shell does not
/// expand anything inside the JSON.
pub fn curl_command(url: &str, body: &str) -> String {
    format!(
        "curl -sS -X POST {} -H 'Content-Type: application/json' --data {}",
        shell_single(url),
        shell_single(body)
    )
}

fn cap(value: &str, max_chars: usize) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    cleaned.trim().chars().take(max_chars).collect()
}

fn shell_single(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_and_control_chars_match_the_endpoint() {
        let payload = Payload::from_input(&Input {
            agent: "  Agent\nName  ",
            used_for: "used",
            verdict: &"v".repeat(CAP_VERDICT + 50),
            version: "xerj v1",
            os: "linux x86_64",
            indexed: &"i".repeat(500),
            notes: &"n".repeat(5000),
        });
        assert_eq!(payload.agent, "Agent Name");
        assert_eq!(payload.verdict.chars().count(), CAP_VERDICT);
        assert_eq!(payload.indexed.chars().count(), CAP_INDEXED);
        assert_eq!(payload.notes.chars().count(), CAP_NOTES);
        assert!(payload.missing_required().is_empty());
    }

    #[test]
    fn empty_required_fields_are_named() {
        let payload = Payload::from_input(&Input {
            agent: "  ",
            used_for: "",
            verdict: "ok",
            version: "",
            os: "",
            indexed: "",
            notes: "",
        });
        assert_eq!(payload.missing_required(), ["agent", "used_for"]);
    }

    #[test]
    fn curl_quotes_single_quotes_in_the_body() {
        let curl = curl_command("http://127.0.0.1:9/api/field-report", r#"{"agent":"a'b"}"#);
        assert!(curl.contains("http://127.0.0.1:9/api/field-report"));
        assert!(curl.contains("'\\''"), "{curl}");
        assert!(curl.contains("Content-Type: application/json"));
    }
}
