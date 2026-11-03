//! Ingest-time labelling through `/_decide` (#1062): `xerj autoindex
//! <folder> --label <question-set.json>` sends every extracted record's
//! payload through the node's decide endpoint and stamps the answers onto
//! the document before it is bulk-indexed.
//!
//! The question-set file is the `noul`/`choice` vocabulary the System One
//! surface already uses (`POST /v1/systemone`), carried in a file so the
//! operator's judgement policy lives beside the corpus instead of in a
//! shell-escaped command line:
//!
//! ```json
//! {
//!   "decide": { "index": "mail-history", "k": 10 },
//!   "questions": [
//!     { "id": "spam",   "type": "noul",   "positive": "spam",
//!       "question": "Is this message spam? {{body}}" },
//!     { "id": "triage", "type": "choice", "options": ["billing", "tech"],
//!       "question": "Which team handles this? {{body}}" }
//!   ]
//! }
//! ```
//!
//! * `{{field}}` in a question resolves against the RECORD's own fields at
//!   ingest time (raw substitution — prose for a judge, not JSON splicing).
//! * A `noul` is one `/_decide` call; the label written is the endpoint's
//!   answer (`<positive>` or `not <positive>`), `null` when it abstains.
//! * A `choice` is one `/_decide` call PER OPTION, each as the binary "is it
//!   `<option>` vs not" vote the endpoint exposes; the written label is the
//!   argmax option and `label_p` that option's share. This is an argmax over
//!   independent binary votes, NOT a calibrated distribution over the
//!   options — the probabilities do not sum to one, and calibration is #1063.
//!
//! Fields written per question `q`: `label_{q}` and `label_{q}_p` (raw,
//! uncalibrated). A single-question set ALSO writes the bare `label` /
//! `label_p` the issue names. A failed `/_decide` call fails the run — an
//! unlabelled index the operator believes is labelled is the accepted-and-
//! ignored defect class, not a warning to be retried past.
//!
//! Cost, stated plainly: labelling is an INGEST-time cost — one blocking
//! `/_decide` round trip per noul (per option, for a choice) per record,
//! paid while the bulk is being prepared, never at idle. There is no idle
//! cost at all: with `--label` absent (the default) this module is not
//! constructed. The end-to-end test measures and prints documents labelled
//! per second against a loopback endpoint.

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};

use crate::esclient::Es;

/// A `noul`'s two sides and a `choice`'s options, straight off the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionKind {
    Noul { positive: String },
    Choice { options: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub id: String,
    pub kind: QuestionKind,
    pub question: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionSet {
    /// Judgement-history index for the decide vote; may be empty, which
    /// asks the node's local tier (the server enforces which it can do).
    pub decide_index: String,
    pub k: Option<u64>,
    pub questions: Vec<Question>,
}

/// The System One API documents at most 255 options per `choice`.
const MAX_CHOICE_OPTIONS: usize = 255;

impl QuestionSet {
    /// Load and validate a question-set file. Everything that can be
    /// refused is refused HERE, before a single byte of the folder is
    /// walked: a set that fails validation halfway through a run would
    /// leave a half-labelled corpus.
    pub fn load(path: &std::path::Path) -> Result<QuestionSet> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("--label: reading question set {}", path.display()))?;
        let value: Value = serde_json::from_str(&raw)
            .with_context(|| format!("--label: parsing {} as JSON", path.display()))?;
        let obj = value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("--label: question set must be a JSON object"))?;

        let decide = obj.get("decide").cloned().unwrap_or_else(|| json!({}));
        let decide_index = decide
            .get("index")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let k = match decide.get("k") {
            None => None,
            Some(v) => Some(
                v.as_u64()
                    .filter(|k| (1..=100).contains(k))
                    .ok_or_else(|| {
                        anyhow::anyhow!("--label: decide.k must be an integer in 1..=100")
                    })?,
            ),
        };

        let questions_value = obj
            .get("questions")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("--label: question set needs a \"questions\" array"))?;
        if questions_value.is_empty() {
            bail!("--label: question set has no questions");
        }
        let mut questions = Vec::with_capacity(questions_value.len());
        let mut seen_ids = std::collections::BTreeSet::new();
        for (n, q) in questions_value.iter().enumerate() {
            let question = Self::parse_question(n, q)
                .with_context(|| format!("--label: question #{}", n + 1))?;
            if !seen_ids.insert(question.id.clone()) {
                bail!("--label: question id {:?} appears twice", question.id);
            }
            questions.push(question);
        }
        Ok(QuestionSet {
            decide_index,
            k,
            questions,
        })
    }

    fn parse_question(n: usize, q: &Value) -> Result<Question> {
        let obj = q
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("must be an object"))?;
        let id = obj
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("needs a non-empty string id"))?
            .to_string();
        // Field names must survive as index fields: idents only.
        if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            bail!(
                "id {:?} must be [a-z0-9_]: it becomes the label_<id> field name",
                id
            );
        }
        let kind = match obj.get("type").and_then(Value::as_str) {
            Some("noul") => QuestionKind::Noul {
                positive: obj
                    .get("positive")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .ok_or_else(|| {
                        anyhow::anyhow!("a noul needs a non-empty string `positive` label")
                    })?
                    .to_string(),
            },
            Some("choice") => {
                let options: Vec<String> = obj
                    .get("options")
                    .and_then(Value::as_array)
                    .ok_or_else(|| anyhow::anyhow!("a choice needs an `options` array"))?
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect();
                if options.is_empty() || options.len() > MAX_CHOICE_OPTIONS {
                    bail!(
                        "a choice needs 1..{MAX_CHOICE_OPTIONS} options (got {})",
                        options.len()
                    );
                }
                QuestionKind::Choice { options }
            }
            other => bail!(
                "type must be \"noul\" or \"choice\" (got {})",
                other.unwrap_or("<missing>")
            ),
        };
        let question = obj
            .get("question")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("needs a non-empty string `question`"))?
            .to_string();
        let _ = n;
        Ok(Question { id, kind, question })
    }
}

/// Stamp labels onto records through the node's `/_decide` endpoint.
pub struct Labeler {
    es: Es,
    set: QuestionSet,
    /// Decide calls made / records labelled, for the run report.
    calls: std::sync::atomic::AtomicU64,
    records: std::sync::atomic::AtomicU64,
}

impl Labeler {
    pub fn new(es: Es, set: QuestionSet) -> Self {
        Self {
            es,
            set,
            calls: std::sync::atomic::AtomicU64::new(0),
            records: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// (decide calls, records labelled) so far.
    pub fn counters(&self) -> (u64, u64) {
        (
            self.calls.load(std::sync::atomic::Ordering::Relaxed),
            self.records.load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// Decide one record. Returns the `(field, value)` pairs to stamp onto
    /// the document — `label`/`label_p` for a single-question set plus
    /// `label_<id>`/`label_<id>_p` for every question.
    pub fn label_fields(&self, record: &Map<String, Value>) -> Result<Vec<(String, Value)>> {
        // Only a single-question set writes the bare `label`/`label_p` the
        // issue names; with several questions they would be ambiguous.
        let primary = self.set.questions.len() == 1;
        let mut out = Vec::new();
        for q in &self.set.questions {
            let rendered = render_question(&q.question, record);
            match &q.kind {
                QuestionKind::Noul { positive } => {
                    let answer = self.decide(&rendered, positive)?;
                    let (label, p) = match &answer {
                        DecideAnswer::Label { label, p } => (Value::String(label.clone()), *p),
                        DecideAnswer::Abstain { p } => (Value::Null, *p),
                    };
                    if primary {
                        out.push(("label".into(), label.clone()));
                        out.push(("label_p".into(), json!(round6(p))));
                    }
                    out.push((format!("label_{}", q.id), label));
                    out.push((format!("label_{}_p", q.id), json!(round6(p))));
                }
                QuestionKind::Choice { options } => {
                    // Argmax over per-option binary votes. An option's share
                    // when it LOST its binary vote is the complement of the
                    // returned confidence, not the returned confidence.
                    let mut best: Option<(String, f64)> = None;
                    for option in options {
                        let answer = self.decide(&rendered, option)?;
                        let p = match answer {
                            DecideAnswer::Label { label, p } if label == *option => p,
                            DecideAnswer::Label { p, .. } => 1.0 - p,
                            DecideAnswer::Abstain { p } => p,
                        };
                        if best.as_ref().is_none_or(|(_, bp)| p > *bp) {
                            best = Some((option.clone(), p));
                        }
                    }
                    let (option, p) = best.expect("a choice has >=1 option");
                    if primary {
                        out.push(("label".into(), Value::String(option.clone())));
                        out.push(("label_p".into(), json!(round6(p))));
                    }
                    out.push((format!("label_{}", q.id), Value::String(option)));
                    out.push((format!("label_{}_p", q.id), json!(round6(p))));
                }
            }
        }
        self.records
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(out)
    }

    /// One binary `/_decide` vote.
    fn decide(&self, question: &str, positive: &str) -> Result<DecideAnswer> {
        let mut body = json!({
            "question": question,
            "positive_label": positive,
        });
        if !self.set.decide_index.is_empty() {
            body["index"] = json!(self.set.decide_index);
        }
        if let Some(k) = self.set.k {
            body["k"] = json!(k);
        }
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (status, value) = self
            .es
            .request_json("POST", "/_decide", Some(&body))
            .map_err(|e| anyhow::anyhow!("/_decide {positive:?}: {e:#}"))?;
        if status != 200 {
            let reason = value
                .pointer("/error/reason")
                .and_then(Value::as_str)
                .unwrap_or("no reason in body");
            bail!(
                "/_decide for label {positive:?} answered HTTP {status}: {reason} — the run is \
                 aborted rather than indexing the document unlabelled"
            )
        }
        let confidence = value
            .get("confidence")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let abstain = value
            .get("abstain")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let label = value.get("label").and_then(Value::as_str);
        if abstain || label.is_none() {
            // Abstention is an answer: label null, raw p carried so the
            // corpus keeps the vote's strength for later calibration (#1063).
            return Ok(DecideAnswer::Abstain { p: confidence });
        }
        Ok(DecideAnswer::Label {
            label: label.unwrap_or("").to_string(),
            p: confidence,
        })
    }
}

enum DecideAnswer {
    Label { label: String, p: f64 },
    Abstain { p: f64 },
}

/// Render a question's `{{field}}` placeholders from the record's fields.
pub(crate) fn render_question(template: &str, record: &Map<String, Value>) -> String {
    const MAX_FIELD_CHARS: usize = 100_000;
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let name = &after[..end];
                match record.get(name) {
                    Some(Value::String(s)) => {
                        out.push_str(&s.chars().take(MAX_FIELD_CHARS).collect::<String>())
                    }
                    Some(Value::Number(n)) => out.push_str(&n.to_string()),
                    Some(Value::Bool(b)) => out.push_str(if *b { "true" } else { "false" }),
                    _ => {}
                }
                rest = &after[end + 2..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

fn round6(p: f64) -> f64 {
    (p * 1e6).round() / 1e6
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpStream;
    use std::sync::{Arc, Mutex};

    fn set(json: &str) -> QuestionSet {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("questions.json");
        std::fs::write(&path, json).unwrap();
        QuestionSet::load(&path).unwrap()
    }

    #[test]
    fn question_set_loads_noul_and_choice() {
        let s = set(r#"{
                "decide": { "index": "hist", "k": 10 },
                "questions": [
                  { "id": "spam", "type": "noul", "positive": "spam",
                    "question": "Is this spam? {{body}}" },
                  { "id": "triage", "type": "choice", "options": ["billing", "tech"],
                    "question": "Team? {{body}}" }
                ]
            }"#);
        assert_eq!(s.decide_index, "hist");
        assert_eq!(s.k, Some(10));
        assert_eq!(s.questions.len(), 2);
        assert_eq!(
            s.questions[0].kind,
            QuestionKind::Noul {
                positive: "spam".into()
            }
        );
        assert_eq!(
            s.questions[1].kind,
            QuestionKind::Choice {
                options: vec!["billing".into(), "tech".into()]
            }
        );
    }

    #[test]
    fn question_set_refuses_what_would_break_midrun() {
        let cases = [
            (r#"{}"#, "questions"),
            (r#"{"questions": []}"#, "no questions"),
            (
                r#"{"questions": [{"id": "a", "type": "noul", "positive": "x", "question": "q"}]}"#,
                "must NOT be refused",
            ),
            (
                r#"{"questions": [{"id": "a", "type": "score", "question": "q"}]}"#,
                "noul\" or \"choice",
            ),
            (
                r#"{"questions": [{"id": "a", "type": "noul", "question": "q"}]}"#,
                "positive",
            ),
            (
                r#"{"questions": [{"id": "a b", "type": "noul", "positive": "x", "question": "q"}]}"#,
                "[a-z0-9_]",
            ),
            (
                r#"{"questions": [{"id": "a", "type": "choice", "question": "q"}]}"#,
                "options",
            ),
            (
                r#"{"questions": [
                    {"id": "a", "type": "noul", "positive": "x", "question": "q"},
                    {"id": "a", "type": "noul", "positive": "x", "question": "q"}
                ]}"#,
                "twice",
            ),
        ];
        for (json, needle) in cases {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("q.json");
            std::fs::write(&path, json).unwrap();
            let got = QuestionSet::load(&path);
            if needle == "must NOT be refused" {
                assert!(got.is_ok(), "{json}: {got:?}");
            } else {
                // {:#} — the whole anyhow chain, so the needle can name the
                // root cause rather than the wrapper.
                let err = format!("{:#}", got.expect_err("must be refused"));
                assert!(err.contains(needle), "{json}: expected {needle:?} in {err}");
            }
        }
    }

    #[test]
    fn questions_render_record_fields() {
        let mut record = Map::new();
        record.insert("body".into(), Value::String("wire transfer".into()));
        record.insert("n".into(), json!(3));
        let q = render_question("Is {{body}} (ticket {{n}}, {{missing}})", &record);
        assert_eq!(q, "Is wire transfer (ticket 3, )");
    }

    /// (label-to-return, p, abstain) — the canned vote for one positive label.
    type StubAnswer = (String, f64, bool);
    /// positive label → its canned vote.
    type StubAnswers = std::collections::HashMap<String, StubAnswer>;

    /// A loopback decide endpoint answering canned votes, so the Labeler is
    /// exercised over real HTTP without a node.
    struct DecideStub {
        url: String,
        /// Answers by positive label.
        answers: Arc<Mutex<StubAnswers>>,
        stop: Arc<Mutex<bool>>,
        join: Option<std::thread::JoinHandle<()>>,
    }

    impl DecideStub {
        fn start() -> Self {
            let listener = crate::test_loopback::bind();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let answers = Arc::new(Mutex::new(std::collections::HashMap::new()));
            let stop = Arc::new(Mutex::new(false));
            let (a, s) = (Arc::clone(&answers), Arc::clone(&stop));
            let join = std::thread::spawn(move || loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        let a = Arc::clone(&a);
                        std::thread::spawn(move || serve(stream, &a));
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if *s.lock().unwrap() {
                            break;
                        }
                        std::thread::yield_now();
                    }
                    Err(e) => panic!("decide stub accept: {e}"),
                }
            });
            Self {
                url,
                answers,
                stop,
                join: Some(join),
            }
        }

        fn answer(&self, positive: &str, label: &str, p: f64, abstain: bool) {
            self.answers
                .lock()
                .unwrap()
                .insert(positive.into(), (label.into(), p, abstain));
        }

        fn es(&self) -> Es {
            Es::new(&self.url, None).unwrap()
        }
    }

    impl Drop for DecideStub {
        fn drop(&mut self) {
            *self.stop.lock().unwrap() = true;
            let _ = TcpStream::connect(self.url.trim_start_matches("http://"));
            self.join.take().unwrap().join().unwrap();
        }
    }

    fn serve(mut stream: TcpStream, answers: &Mutex<StubAnswers>) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).unwrap() == 0 {
            return;
        }
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
            if let Some(v) = line
                .to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(str::trim)
            {
                content_length = v.parse().unwrap_or(0);
            }
        }
        let mut body = vec![0; content_length];
        if content_length > 0 {
            reader.read_exact(&mut body).unwrap();
        }
        let value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let positive = value
            .get("positive_label")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let (label, p, abstain) = answers.lock().unwrap().get(&positive).cloned().unwrap_or((
            "unknown".into(),
            0.0,
            true,
        ));
        let resp = json!({
            "label": if abstain { Value::Null } else { Value::String(label) },
            "confidence": p,
            "abstain": abstain,
            "tier": "history",
            "neighbours": [],
            "took_ms": 0,
        });
        let bytes = resp.to_string();
        // write!, not writeln!: writeln's trailing newline would land between
        // the header terminator and the body and corrupt the framed JSON.
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            bytes.len()
        );
        let _ = stream.write_all(bytes.as_bytes());
        let _ = stream.flush();
    }

    #[test]
    fn labeler_stamps_bare_and_per_question_fields() {
        let stub = DecideStub::start();
        stub.answer("spam", "spam", 0.9, false);
        stub.answer("billing", "not billing", 0.8, false); // lost its vote
        stub.answer("tech", "tech", 0.7, false);
        stub.answer("abstain_q", "whatever", 0.3, true);
        let s = set(r#"{"decide": {"index": "hist"},
                "questions": [
                  {"id": "spam", "type": "noul", "positive": "spam", "question": "{{body}}"}
                ]}"#);
        let labeler = Labeler::new(stub.es(), s);
        let mut record = Map::new();
        record.insert("body".into(), Value::String("claim your prize".into()));
        let fields = labeler.label_fields(&record).unwrap();
        let map: std::collections::BTreeMap<String, Value> = fields.into_iter().collect();
        assert_eq!(map["label"], json!("spam"), "single question: bare fields");
        assert_eq!(map["label_p"], json!(0.9));
        assert_eq!(map["label_spam"], json!("spam"));
        assert_eq!(map["label_spam_p"], json!(0.9));

        // A choice: argmax over per-option binary votes. billing LOST its
        // own vote (conf 0.8 for "not billing" → share 0.2); tech WON with
        // 0.7 → label tech.
        let s = set(r#"{"questions": [
                {"id": "triage", "type": "choice", "options": ["billing", "tech"],
                 "question": "{{body}}"}]}"#);
        let labeler = Labeler::new(stub.es(), s);
        let fields = labeler.label_fields(&record).unwrap();
        let map: std::collections::BTreeMap<String, Value> = fields.into_iter().collect();
        assert_eq!(map["label"], json!("tech"));
        assert_eq!(map["label_p"], json!(0.7));
        assert_eq!(map["label_triage"], json!("tech"));
        assert_eq!(map["label_triage_p"], json!(0.7));

        // Abstention: label null, raw p carried.
        let s = set(r#"{"questions": [
                {"id": "abstain_q", "type": "noul", "positive": "abstain_q",
                 "question": "{{body}}"}]}"#);
        let labeler = Labeler::new(stub.es(), s);
        let fields = labeler.label_fields(&record).unwrap();
        let map: std::collections::BTreeMap<String, Value> = fields.into_iter().collect();
        assert_eq!(map["label"], Value::Null);
        assert_eq!(map["label_p"], json!(0.3));

        // Multi-question sets never write the bare fields.
        let s = set(r#"{"questions": [
                {"id": "a", "type": "noul", "positive": "spam", "question": "{{body}}"},
                {"id": "b", "type": "noul", "positive": "spam", "question": "{{body}}"}]}"#);
        let labeler = Labeler::new(stub.es(), s);
        let fields = labeler.label_fields(&record).unwrap();
        assert!(!fields.iter().any(|(k, _)| k == "label"), "{fields:?}");
        let (calls, records) = labeler.counters();
        assert_eq!((calls, records), (2, 1));
    }
}
