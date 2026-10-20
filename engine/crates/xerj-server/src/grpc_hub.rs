//! Corpus Hub v2 — the gRPC `XerjHub` service (plan §7,
//! `docs/CORPUS_HUB_V2_IMPLEMENTATION.md`; wire contract and mapping in
//! `docs/hub-v2-grpc-mcp-notes.md`).
//!
//! A thin adapter, and nothing else. Each RPC converts its request into the
//! arguments of a function in `xerj_api::hub_api` and the [`Answer`] that comes
//! back into a [`pb::HubAnswer`]:
//!
//! | RPC | runs | which is also |
//! |---|---|---|
//! | `Def` | [`hub_api::def_answer`] | `GET /_hub/v1/def/{slug}/{symbol}` (the same `def_core`) |
//! | `Ask` | [`hub_api::ask_answer`] | `xerj ask`'s flow (`xerj_hub::ask::ask`) over the cores behind `/_hub/v1/def` and `/_hub/v1/code` |
//! | `Catalogue` | [`hub_api::catalogue_signed`] | `GET /_hub/v1/catalogue`, plus the detached `.sig` |
//!
//! So every check the HTTP routes make — the catalogue lookup, the licence
//! decision (an `excluded` corpus is never served; an `approach-only` one tags
//! every hit with its licence warning), the 512-byte input cap, the `k` clamp,
//! the in-process node pinned to one corpus build, the `xc-*` visibility guard,
//! the query-concurrency cap and timeout, the per-client rate limit — applies
//! here unchanged, because it is the same code.
//!
//! It is also the same STATE: the service holds the very [`HubShared`] the
//! ES-compat router's `/_hub/v1/*` routes were built over, so a client has one
//! rate-limit budget across both listeners (not one per listener), and the
//! client is read as the HTTP layer reads it
//! ([`xerj_api::share::client_source`]: the TCP peer, or `x-forwarded-for`
//! metadata only when the peer is a declared `server.trusted_proxies` entry).
//!
//! Read-only: none of the three RPCs writes, pulls, evicts or deletes anything
//! (`ask_answer` runs the flow offline with a backend that refuses to pull).
//!
//! # Authentication
//!
//! The service sits behind the same [`super::grpc`] interceptor as
//! `XerjSearch`: an RPC without a valid key (when auth is on) never reaches it.
//! The caller's principal then becomes the engine visibility rule the hub's
//! in-process node runs under, exactly as `authz_middleware` installs it for
//! the HTTP routes, so a key scoped to `tenant-a-*` is refused a corpus here
//! as it is there. `hub.public` does not open these RPCs: it is an HTTP-route
//! setting, and the gRPC listener always authenticates.
//!
//! # The proto mapping (frozen; see the notes)
//!
//! Enums travel as their serde strings (`licence_class` kebab-case,
//! `answered_from` / `mode` snake_case); `Option<String>` ↔ proto3 `string` is
//! `None` ↔ `""`. [`answer_to_pb`] / [`answer_from_pb`] are the only place the
//! conversion is written.

use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::Serialize;
use tonic::{Request, Response, Status};

use xerj_api::auth::Principal;
use xerj_api::hub_api::{self, HubAsk, HubRefusal, HubShared};
use xerj_common::hub::{Answer, AnswerMode, Hit};
use xerj_engine::index_guard;

use crate::grpc::pb;
use pb::xerj_hub_server::XerjHub;

/// The `XerjHub` service. Holds the node's [`HubShared`] (query slots,
/// rate-limit budget, catalogue cache) — the one the ES-compat router's hub
/// routes hold, never a second one.
#[derive(Clone)]
pub struct HubService {
    shared: Arc<HubShared>,
}

impl HubService {
    pub fn new(shared: Arc<HubShared>) -> Self {
        Self { shared }
    }

    /// Who is calling and from where, with the rate limit counted. The visibility
    /// rule is `None` for the superuser (open mode, the admin key), exactly as
    /// `authz_middleware` installs none for it.
    #[allow(clippy::result_large_err)] // `Err` is the RPC's own status
    fn admit<T>(&self, request: &Request<T>) -> Result<Caller, Status> {
        let principal = request
            .extensions()
            .get::<Principal>()
            .cloned()
            .unwrap_or(Principal::Denied);
        if matches!(principal, Principal::Denied) {
            return Err(Status::unauthenticated(
                "missing or invalid API key in authorization metadata",
            ));
        }
        if principal.is_share_guest() {
            return Err(Status::permission_denied(
                "a share link reads the index it was issued for, not the hub",
            ));
        }
        let forwarded: Vec<&str> = request
            .metadata()
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        self.shared
            .admit_peer(request.remote_addr().map(|a| a.ip()), &forwarded)
            .map_err(refusal_status)?;
        let visibility =
            (!principal.is_superuser()).then(|| xerj_api::authz::visibility_for(&principal));
        // The in-process node forwards the caller's credential the way the HTTP
        // route forwards its header; it has no auth layer of its own, so this is
        // informational, never a grant.
        let auth = request
            .metadata()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| axum::http::HeaderValue::from_str(v).ok());
        Ok(Caller { visibility, auth })
    }
}

struct Caller {
    visibility: Option<Arc<dyn index_guard::IndexVisibility>>,
    auth: Option<axum::http::HeaderValue>,
}

#[tonic::async_trait]
impl XerjHub for HubService {
    async fn ask(
        &self,
        request: Request<pb::HubAskRequest>,
    ) -> Result<Response<pb::HubAnswer>, Status> {
        let caller = self.admit(&request)?;
        let req = request.into_inner();
        let ask = ask_from_pb(req)?;
        let answer = index_guard::scoped_opt(
            caller.visibility,
            hub_api::ask_answer(&self.shared, ask, caller.auth),
        )
        .await
        .map_err(refusal_status)?;
        Ok(Response::new(answer_to_pb(&answer)))
    }

    async fn def(
        &self,
        request: Request<pb::HubDefRequest>,
    ) -> Result<Response<pb::HubAnswer>, Status> {
        let caller = self.admit(&request)?;
        let req = request.into_inner();
        let answer = index_guard::scoped_opt(
            caller.visibility,
            hub_api::def_answer(
                &self.shared,
                &req.slug,
                &req.symbol,
                req.k as usize,
                caller.auth,
            ),
        )
        .await
        .map_err(refusal_status)?;
        Ok(Response::new(answer_to_pb(&answer)))
    }

    async fn catalogue(
        &self,
        request: Request<pb::HubCatalogueRequest>,
    ) -> Result<Response<pb::HubCatalogueResponse>, Status> {
        self.admit(&request)?;
        let signed = hub_api::catalogue_signed(&self.shared)
            .await
            .map_err(refusal_status)?;
        Ok(Response::new(pb::HubCatalogueResponse {
            catalogue_json: signed.bytes.to_vec(),
            signature: signed.signature,
        }))
    }
}

// ── request decoding ─────────────────────────────────────────────────────────

/// `HubAskRequest` → [`HubAsk`]: `corpus` and `mode` empty = auto, `max_tokens`
/// 0 = none. The question, the corpus and the budget are validated by
/// `ask_answer` itself (the HTTP limits); only the mode's spelling is checked
/// here, because it is this surface's own string.
#[allow(clippy::result_large_err)]
fn ask_from_pb(req: pb::HubAskRequest) -> Result<HubAsk, Status> {
    let mode = if req.mode.is_empty() {
        None
    } else {
        Some(enum_from::<AnswerMode>(&req.mode).map_err(|_| {
            Status::invalid_argument(format!(
                "mode must be def, how or files (or empty for auto), got [{}]",
                hub_api::truncate_chars(&req.mode, 32)
            ))
        })?)
    };
    Ok(HubAsk {
        question: req.question,
        corpus: (!req.corpus.is_empty()).then_some(req.corpus),
        mode,
        max_tokens: (req.max_tokens > 0).then_some(u64::from(req.max_tokens)),
    })
}

/// The route's HTTP status as a gRPC code, with its reason, and `Retry-After`
/// carried as `retry-after` metadata.
fn refusal_status(r: HubRefusal) -> Status {
    use tonic::Code;
    let code = match r.status.as_u16() {
        400 => Code::InvalidArgument,
        401 => Code::Unauthenticated,
        403 => Code::PermissionDenied,
        404 => Code::NotFound,
        // `Ask` with more than one candidate corpus: re-ask naming one.
        409 => Code::FailedPrecondition,
        429 => Code::ResourceExhausted,
        503 => Code::Unavailable,
        504 => Code::DeadlineExceeded,
        _ => Code::Internal,
    };
    let mut status = Status::new(code, r.reason);
    if let Some(v) = r.retry_after.and_then(|v| v.parse().ok()) {
        status.metadata_mut().insert("retry-after", v);
    }
    status
}

// ── the frozen mapping: Answer ↔ HubAnswer ───────────────────────────────────

/// The serde string of a unit enum variant (`approach-only`, `local_pack`, …).
fn enum_str<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        // Every hub enum is a unit enum with a string form; unreachable.
        other => unreachable!("hub enum did not serialise to a string: {other:?}"),
    }
}

fn enum_from<T: DeserializeOwned>(s: &str) -> Result<T, String> {
    serde_json::from_value(serde_json::Value::String(s.to_string()))
        .map_err(|e| format!("[{s}]: {e}"))
}

/// `None` → `""` on the way out.
fn opt_out(v: &Option<String>) -> String {
    v.clone().unwrap_or_default()
}

/// `""` → `None` on the way in.
#[cfg_attr(not(test), allow(dead_code))] // the decode half; the server only encodes
fn opt_in(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// [`Answer`] → [`pb::HubAnswer`] (fields 1–11, hits 1–7).
pub fn answer_to_pb(a: &Answer) -> pb::HubAnswer {
    pb::HubAnswer {
        corpus: a.corpus.clone(),
        pin_sha: a.pin_sha.clone(),
        pin_date: a.pin_date.clone(),
        age_days: a.age_days,
        licence_spdx: a.licence_spdx.clone(),
        licence_class: enum_str(&a.licence_class),
        answered_from: enum_str(&a.answered_from),
        mode: enum_str(&a.mode),
        hits: a
            .hits
            .iter()
            .map(|h| pb::HubHit {
                file: h.file.clone(),
                line: h.line,
                kind: h.kind.clone(),
                text: h.text.clone(),
                value: opt_out(&h.value),
                score: h.score,
                licence_warning: opt_out(&h.licence_warning),
            })
            .collect(),
        truncated: a.truncated,
        warnings: a.warnings.clone(),
    }
}

/// [`pb::HubAnswer`] → [`Answer`]: what a Rust gRPC client decodes with. An
/// enum string this build does not know is an error, not a default. The server
/// only encodes; the decode half is the client's, pinned here by the tests.
#[cfg_attr(not(test), allow(dead_code))]
pub fn answer_from_pb(p: &pb::HubAnswer) -> Result<Answer, String> {
    Ok(Answer {
        corpus: p.corpus.clone(),
        pin_sha: p.pin_sha.clone(),
        pin_date: p.pin_date.clone(),
        age_days: p.age_days,
        licence_spdx: p.licence_spdx.clone(),
        licence_class: enum_from(&p.licence_class)?,
        answered_from: enum_from(&p.answered_from)?,
        mode: enum_from(&p.mode)?,
        hits: p
            .hits
            .iter()
            .map(|h| Hit {
                file: h.file.clone(),
                line: h.line,
                kind: h.kind.clone(),
                text: h.text.clone(),
                value: opt_in(&h.value),
                score: h.score,
                licence_warning: opt_in(&h.licence_warning),
            })
            .collect(),
        truncated: p.truncated,
        warnings: p.warnings.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use xerj_common::hub::{AnsweredFrom, LicenceClass};

    fn hit(value: Option<&str>, warning: Option<&str>) -> Hit {
        Hit {
            file: "src/limits.h".into(),
            line: 5,
            kind: "define".into(),
            text: "#define MINI_LIMIT 128".into(),
            value: value.map(str::to_string),
            score: 3.25,
            licence_warning: warning.map(str::to_string),
        }
    }

    fn answer(class: LicenceClass, from: AnsweredFrom, mode: AnswerMode) -> Answer {
        Answer {
            corpus: "mini-c".into(),
            pin_sha: "0774d05537f9762f838f7ab541b7765f1a729cb5".into(),
            pin_date: "2026-10-02".into(),
            age_days: 13,
            licence_spdx: "GPL-3.0-only".into(),
            licence_class: class,
            answered_from: from,
            mode,
            hits: vec![
                hit(
                    Some("128"),
                    Some("approach-only licence (GPL-3.0-only): read, do not copy"),
                ),
                hit(None, None),
            ],
            truncated: true,
            warnings: vec!["one".into(), "two".into()],
        }
    }

    /// Every field, every enum variant, both `Option` states: out to the wire
    /// form the notes freeze and back to an identical `Answer`.
    #[test]
    fn answer_round_trips_through_the_proto_field_by_field() {
        let classes = [
            (LicenceClass::AdaptWithAttribution, "adapt-with-attribution"),
            (LicenceClass::ApproachOnly, "approach-only"),
            (LicenceClass::Excluded, "excluded"),
        ];
        let froms = [
            (AnsweredFrom::LocalPack, "local_pack"),
            (AnsweredFrom::Hosted, "hosted"),
            (AnsweredFrom::LocalIndex, "local_index"),
        ];
        let modes = [
            (AnswerMode::Def, "def"),
            (AnswerMode::How, "how"),
            (AnswerMode::Files, "files"),
        ];
        for (i, (class, class_s)) in classes.iter().enumerate() {
            let (from, from_s) = froms[i];
            let (mode, mode_s) = modes[i];
            let a = answer(*class, from, mode);
            let p = answer_to_pb(&a);

            assert_eq!(p.corpus, "mini-c");
            assert_eq!(p.pin_sha, a.pin_sha);
            assert_eq!(p.pin_date, "2026-10-02");
            assert_eq!(p.age_days, 13);
            assert_eq!(p.licence_spdx, "GPL-3.0-only");
            assert_eq!(p.licence_class, *class_s, "serde kebab-case");
            assert_eq!(p.answered_from, from_s, "serde snake_case, not label()");
            assert_ne!(p.answered_from, from.label());
            assert_eq!(p.mode, mode_s);
            assert!(p.truncated);
            assert_eq!(p.warnings, vec!["one", "two"]);
            assert_eq!(p.hits.len(), 2);
            let (h0, h1) = (&p.hits[0], &p.hits[1]);
            assert_eq!(
                (h0.file.as_str(), h0.line, h0.kind.as_str()),
                ("src/limits.h", 5, "define")
            );
            assert_eq!(h0.text, "#define MINI_LIMIT 128");
            assert_eq!(h0.value, "128");
            assert_eq!(h0.score, 3.25);
            assert_eq!(
                h0.licence_warning,
                "approach-only licence (GPL-3.0-only): read, do not copy"
            );
            assert_eq!(h1.value, "", "None -> \"\"");
            assert_eq!(h1.licence_warning, "", "None -> \"\"");

            let back = answer_from_pb(&p).expect("decodes");
            assert_eq!(
                serde_json::to_value(&back).unwrap(),
                serde_json::to_value(&a).unwrap(),
                "the round trip is lossless"
            );
            assert_eq!(back.hits[1].value, None, "\"\" -> None");
            assert_eq!(back.hits[1].licence_warning, None, "\"\" -> None");
        }
    }

    #[test]
    fn an_unknown_enum_string_is_refused_not_defaulted() {
        let mut p = answer_to_pb(&answer(
            LicenceClass::ApproachOnly,
            AnsweredFrom::Hosted,
            AnswerMode::Def,
        ));
        p.licence_class = "Approach Only".into();
        assert!(answer_from_pb(&p).is_err());
        let mut p = answer_to_pb(&answer(
            LicenceClass::ApproachOnly,
            AnsweredFrom::Hosted,
            AnswerMode::Def,
        ));
        p.answered_from = "hosted hub".into(); // the label, not the wire form
        assert!(answer_from_pb(&p).is_err());
    }

    #[test]
    fn ask_requests_decode_empty_as_auto_and_refuse_a_bad_mode() {
        let ask = ask_from_pb(pb::HubAskRequest {
            question: "q".into(),
            corpus: String::new(),
            mode: String::new(),
            max_tokens: 0,
        })
        .unwrap();
        assert_eq!((ask.corpus, ask.mode, ask.max_tokens), (None, None, None));
        let ask = ask_from_pb(pb::HubAskRequest {
            question: "q".into(),
            corpus: "mini-c".into(),
            mode: "files".into(),
            max_tokens: 200,
        })
        .unwrap();
        assert_eq!(ask.corpus.as_deref(), Some("mini-c"));
        assert_eq!(ask.mode, Some(AnswerMode::Files));
        assert_eq!(ask.max_tokens, Some(200));
        let err = ask_from_pb(pb::HubAskRequest {
            question: "q".into(),
            corpus: String::new(),
            mode: "semantic".into(),
            max_tokens: 0,
        })
        .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn refusals_keep_their_meaning_as_grpc_codes() {
        use axum::http::StatusCode;
        let r = |status: StatusCode| HubRefusal {
            status,
            reason: "why".into(),
            retry_after: Some("3".into()),
        };
        assert_eq!(
            refusal_status(r(StatusCode::NOT_FOUND)).code(),
            tonic::Code::NotFound
        );
        assert_eq!(
            refusal_status(r(StatusCode::BAD_REQUEST)).code(),
            tonic::Code::InvalidArgument
        );
        assert_eq!(
            refusal_status(r(StatusCode::CONFLICT)).code(),
            tonic::Code::FailedPrecondition
        );
        let limited = refusal_status(r(StatusCode::TOO_MANY_REQUESTS));
        assert_eq!(limited.code(), tonic::Code::ResourceExhausted);
        assert_eq!(limited.metadata().get("retry-after").unwrap(), "3");
        assert_eq!(limited.message(), "why");
    }

    // ── the rate limit: one budget per client, across HTTP and gRPC ──

    mod rate_limit {
        use super::super::*;
        use axum::body::Body;
        use std::net::SocketAddr;
        use tonic::transport::server::TcpConnectInfo;
        use tower::ServiceExt;
        use xerj_api::hub_api::RATE_LIMIT;

        fn state(dir: &std::path::Path, trusted: &[&str]) -> xerj_api::AppState {
            let mut config = xerj_common::config::Config::default();
            config.server.data_dir = dir.to_string_lossy().into_owned();
            config.server.trusted_proxies = trusted.iter().map(|s| s.to_string()).collect();
            let metrics = xerj_common::metrics::Metrics::new().unwrap();
            let engine = xerj_engine::Engine::new(config.clone()).unwrap();
            xerj_api::AppState::new(config, engine, metrics)
        }

        /// A request as tonic's transport hands it to the service: the
        /// interceptor's principal installed, the TCP peer recorded, and
        /// whatever `x-forwarded-for` metadata the caller chose to send.
        fn from(peer: Option<&str>, xff: Option<&str>) -> Request<()> {
            let mut r = Request::new(());
            r.extensions_mut().insert(Principal::Superuser);
            if let Some(peer) = peer {
                r.extensions_mut().insert(TcpConnectInfo {
                    local_addr: None,
                    remote_addr: Some(peer.parse::<SocketAddr>().unwrap()),
                });
            }
            if let Some(xff) = xff {
                r.metadata_mut()
                    .insert("x-forwarded-for", xff.parse().unwrap());
            }
            r
        }

        fn admitted(svc: &HubService, r: &Request<()>) -> Result<(), tonic::Code> {
            svc.admit(r).map(|_| ()).map_err(|s| s.code())
        }

        /// The two listeners wired as `main.rs` wires them: one `HubShared`
        /// for the ES-compat router and the gRPC service.
        #[tokio::test]
        async fn http_and_grpc_draw_on_one_budget() {
            let dir = tempfile::tempdir().unwrap();
            let state = state(dir.path(), &[]);
            let shared = HubShared::new(state.clone());
            let app = xerj_api::router::build_es_compat_router_with_hub(state, shared.clone());
            let svc = HubService::new(shared);
            // `oneshot` carries no ConnectInfo, so HTTP keys these on
            // `unknown`; a gRPC request with no peer keys on `unknown` too.
            for i in 0..RATE_LIMIT {
                let resp = app
                    .clone()
                    .oneshot(
                        axum::http::Request::get("/_hub/v1/catalogue")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_ne!(resp.status().as_u16(), 429, "HTTP request {i}");
            }
            assert_eq!(
                admitted(&svc, &from(None, None)),
                Err(tonic::Code::ResourceExhausted),
                "{RATE_LIMIT} HTTP requests spent the client's budget; gRPC must not grant \
                 a second one"
            );
        }

        #[tokio::test]
        async fn a_trusted_proxy_is_keyed_on_the_forwarded_client() {
            let dir = tempfile::tempdir().unwrap();
            let svc = HubService::new(HubShared::new(state(dir.path(), &["10.0.0.0/8"])));
            for _ in 0..RATE_LIMIT {
                admitted(&svc, &from(Some("10.1.1.1:5000"), Some("203.0.113.5"))).unwrap();
            }
            assert_eq!(
                admitted(&svc, &from(Some("10.1.1.1:5000"), Some("203.0.113.5"))),
                Err(tonic::Code::ResourceExhausted),
                "the forwarded client is throttled"
            );
            assert_eq!(
                admitted(&svc, &from(Some("10.1.1.1:5001"), Some("203.0.113.6"))),
                Ok(()),
                "another client behind the same sidecar has its own budget"
            );
            // Right-to-left past declared proxies, as the HTTP path reads it.
            assert_eq!(
                admitted(
                    &svc,
                    &from(Some("10.1.1.1:5002"), Some("198.51.100.7, 10.2.2.2"))
                ),
                Ok(()),
            );
        }

        #[tokio::test]
        async fn an_untrusted_peer_cannot_pick_its_bucket() {
            let dir = tempfile::tempdir().unwrap();
            let svc = HubService::new(HubShared::new(state(dir.path(), &["10.0.0.0/8"])));
            for i in 0..RATE_LIMIT {
                let xff = format!("203.0.113.{i}");
                admitted(&svc, &from(Some("192.0.2.9:7000"), Some(&xff))).unwrap();
            }
            assert_eq!(
                admitted(&svc, &from(Some("192.0.2.9:7000"), Some("198.51.100.99"))),
                Err(tonic::Code::ResourceExhausted),
                "a fresh x-forwarded-for from an untrusted peer is ignored"
            );
        }
    }

    // ── end to end: a real node, a restored fixture corpus, a real listener ──

    mod e2e {
        use super::super::*;
        use crate::grpc::pb::xerj_hub_client::XerjHubClient;
        use axum::body::Body;
        use serde_json::{json, Value};
        use std::sync::OnceLock;
        use tower::ServiceExt;
        use xerj_common::hub::{Catalogue, CatalogueEntry, LicenceClass, CATALOGUE_FORMAT_VERSION};

        const PIN_SHA: &str = "0774d05537f9762f838f7ab541b7765f1a729cb5";
        const PIN_DATE: &str = "2026-10-02";
        const ADMIN_KEY: &str = "grpc-hub-test-admin-key";
        /// 64 bytes, hex: what a detached `.sig` file holds.
        const SIG_HEX: &str = "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20\
                               2122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f40";

        struct Fixture {
            catalogue: std::path::PathBuf,
        }

        /// The ledger root is process-global (`XERJ_CODE_HOME`): one fixture,
        /// exported once, as `xerj-api/tests/hub_api.rs` does.
        fn fixture() -> &'static Fixture {
            static FX: OnceLock<Fixture> = OnceLock::new();
            FX.get_or_init(|| {
                let dir = tempfile::tempdir().unwrap().keep();
                let home = dir.join("code-home");
                let led = |slug: &str, spdx: &str, class: &str| {
                    xerj_common::xccode::state::write_hubpack_state(
                        &home,
                        slug,
                        "http://x",
                        &format!("xc-{slug}-b1"),
                        PIN_SHA,
                        PIN_DATE,
                        spdx,
                        class,
                    )
                    .unwrap();
                };
                led("mini-c", "MIT", "adapt-with-attribution");
                led("mini-gpl", "GPL-3.0-only", "approach-only");
                led("mini-closed", "LicenseRef-Closed", "excluded");
                let entry =
                    |slug: &str, spdx: &str, class: LicenceClass, terms: &[&str]| CatalogueEntry {
                        slug: slug.into(),
                        domain: "DEV".into(),
                        topics: vec![],
                        ask_for: format!("fixture corpus {slug}"),
                        pin_sha: PIN_SHA.into(),
                        pin_date: PIN_DATE.into(),
                        refresh_cadence_days: 7,
                        licence_spdx: spdx.into(),
                        licence_class: class,
                        pack_bytes: 1024,
                        pack_url: String::new(),
                        pack_sha256: "0".repeat(64),
                        sig_url: String::new(),
                        dependency_names: vec![],
                        quality_grade: None,
                        term_sketch: xerj_hub::sketch::build(terms.iter().map(|t| t.to_string())),
                    };
                let cat = Catalogue {
                    format_version: CATALOGUE_FORMAT_VERSION,
                    generated_at: "2026-10-14T00:00:00Z".into(),
                    corpora: vec![
                        entry(
                            "mini-c",
                            "MIT",
                            LicenceClass::AdaptWithAttribution,
                            &["MINI_LIMIT", "mini_hash", "frobnicate", "hash"],
                        ),
                        entry(
                            "mini-gpl",
                            "GPL-3.0-only",
                            LicenceClass::ApproachOnly,
                            &["GPL_LIMIT", "copyleft"],
                        ),
                        entry(
                            "mini-closed",
                            "LicenseRef-Closed",
                            LicenceClass::Excluded,
                            &["CLOSED_LIMIT", "secret"],
                        ),
                        entry(
                            "filler-a",
                            "MIT",
                            LicenceClass::AdaptWithAttribution,
                            &["alpha"],
                        ),
                        entry(
                            "filler-b",
                            "MIT",
                            LicenceClass::AdaptWithAttribution,
                            &["bravo"],
                        ),
                    ],
                };
                let catalogue = dir.join("catalogue.json");
                std::fs::write(&catalogue, serde_json::to_vec(&cat).unwrap()).unwrap();
                std::fs::write(dir.join("catalogue.json.sig"), format!("{SIG_HEX}\n")).unwrap();
                std::env::set_var("XERJ_CODE_HOME", &home);
                Fixture { catalogue }
            })
        }

        fn state(dir: &std::path::Path) -> xerj_api::AppState {
            let fx = fixture();
            let mut config = xerj_common::config::Config::default();
            config.server.data_dir = dir.to_string_lossy().into_owned();
            config.storage.wal_sync = xerj_common::config::WalSync::Async;
            config.hub.catalogue_path = fx.catalogue.to_string_lossy().into_owned();
            config.auth.enabled = true;
            config.auth.admin_api_key = ADMIN_KEY.into();
            let metrics = xerj_common::metrics::Metrics::new().unwrap();
            let engine = xerj_engine::Engine::new(config.clone()).unwrap();
            xerj_api::AppState::new(config, engine, metrics)
        }

        async fn http(
            app: &axum::Router,
            method: &str,
            path: &str,
            body: Option<String>,
        ) -> (u16, Value) {
            let mut b = axum::http::Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("ApiKey {ADMIN_KEY}"));
            if body.is_some() {
                b = b.header(
                    "content-type",
                    if path.ends_with("_bulk") {
                        "application/x-ndjson"
                    } else {
                        "application/json"
                    },
                );
            }
            let req = b
                .body(body.map(Body::from).unwrap_or_else(Body::empty))
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            let status = resp.status().as_u16();
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
            (
                status,
                serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            )
        }

        async fn seed(
            app: &axum::Router,
            slug: &str,
            defs: &[(&str, &str, u32, &str, Option<&str>)],
        ) {
            let index = format!("xc-{slug}-b1-defs");
            let mapping = json!({ "mappings": { "properties": {
                "symbol": {"type": "keyword"}, "symbol_text": {"type": "text"},
                "kind": {"type": "keyword"}, "lang": {"type": "keyword"},
                "file": {"type": "keyword"}, "line": {"type": "integer"},
                "text": {"type": "text"}, "value": {"type": "keyword"}
            }}});
            let (st, v) = http(app, "PUT", &format!("/{index}"), Some(mapping.to_string())).await;
            assert!(st < 300, "create {index}: {v}");
            let mut nd = String::new();
            for (symbol, file, line, text, value) in defs {
                nd.push_str("{\"index\":{}}\n");
                nd.push_str(
                    &json!({"symbol": symbol, "symbol_text": symbol, "kind": "define",
                    "lang": "c", "file": file, "line": line, "text": text, "value": value})
                    .to_string(),
                );
                nd.push('\n');
            }
            let (st, v) = http(
                app,
                "POST",
                &format!("/{index}/_bulk?refresh=true"),
                Some(nd),
            )
            .await;
            assert!(st < 300 && v["errors"] == json!(false), "bulk {index}: {v}");
            let docs = format!("xc-{slug}-b1-docs");
            let (st, v) = http(
                app,
                "POST",
                &format!("/{docs}/_bulk?refresh=true"),
                Some(format!(
                    "{{\"index\":{{}}}}\n{}\n",
                    json!({"ax_path": format!("{slug}/hash.c"), "title": "hash.c",
                       "body": "unsigned mini_hash(const char *s) { frobnicate the buffer; }"})
                )),
            )
            .await;
            assert!(st < 300, "bulk {docs}: {v}");
        }

        fn keyed<T>(msg: T) -> tonic::Request<T> {
            let mut r = tonic::Request::new(msg);
            r.metadata_mut().insert(
                "authorization",
                format!("ApiKey {ADMIN_KEY}").parse().unwrap(),
            );
            r
        }

        /// One test, scenarios in order: they share the process-global ledger
        /// root and one node.
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn xerj_hub_rpcs_answer_exactly_like_the_http_routes() {
            let fx = fixture();
            let data = tempfile::tempdir().unwrap();
            let state = state(data.path());
            let shared = HubShared::new(state.clone());
            let app =
                xerj_api::router::build_es_compat_router_with_hub(state.clone(), shared.clone());
            seed(
                &app,
                "mini-c",
                &[(
                    "MINI_LIMIT",
                    "limits.h",
                    5,
                    "#define MINI_LIMIT 128",
                    Some("128"),
                )],
            )
            .await;
            seed(
                &app,
                "mini-gpl",
                &[("GPL_LIMIT", "gpl.h", 3, "#define GPL_LIMIT 7", Some("7"))],
            )
            .await;
            seed(
                &app,
                "mini-closed",
                &[(
                    "CLOSED_LIMIT",
                    "c.h",
                    1,
                    "#define CLOSED_LIMIT 1",
                    Some("1"),
                )],
            )
            .await;

            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            let server = tokio::spawn(crate::grpc::serve_grpc_on(
                listener,
                state.clone(),
                shared,
                async move {
                    let _ = rx.await;
                },
            ));
            let url = format!("http://127.0.0.1:{port}");
            let mut client = None;
            for _ in 0..50 {
                if let Ok(c) = XerjHubClient::connect(url.clone()).await {
                    client = Some(c);
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            let mut client = client.expect("gRPC client connects");

            // No credential: the shared interceptor refuses before the service.
            let err = client
                .def(tonic::Request::new(pb::HubDefRequest {
                    slug: "mini-c".into(),
                    symbol: "MINI_LIMIT".into(),
                    k: 0,
                }))
                .await
                .unwrap_err();
            assert_eq!(err.code(), tonic::Code::Unauthenticated);

            // Def == GET /_hub/v1/def, field by field.
            let grpc = client
                .def(keyed(pb::HubDefRequest {
                    slug: "mini-c".into(),
                    symbol: "MINI_LIMIT".into(),
                    k: 0,
                }))
                .await
                .expect("Def answers")
                .into_inner();
            let (st, body) = http(&app, "GET", "/_hub/v1/def/mini-c/MINI_LIMIT", None).await;
            assert_eq!(st, 200, "{body}");
            let via_http: Answer = serde_json::from_value(body).unwrap();
            assert_eq!(
                grpc,
                answer_to_pb(&via_http),
                "gRPC Def and the HTTP route agree on every field"
            );
            let a = answer_from_pb(&grpc).unwrap();
            assert_eq!(a.corpus, "mini-c");
            assert_eq!(a.hits.len(), 1);
            assert_eq!(a.hits[0].value.as_deref(), Some("128"));
            assert_eq!((a.hits[0].file.as_str(), a.hits[0].line), ("limits.h", 5));
            assert_eq!(grpc.answered_from, "hosted");
            assert_eq!(grpc.mode, "def");
            assert_eq!(
                grpc.hits[0].licence_warning, "",
                "a permissive corpus carries no warning"
            );

            // Approach-only: every hit carries the read-do-not-copy warning.
            let gpl = client
                .def(keyed(pb::HubDefRequest {
                    slug: "mini-gpl".into(),
                    symbol: "GPL_LIMIT".into(),
                    k: 0,
                }))
                .await
                .unwrap()
                .into_inner();
            assert_eq!(gpl.licence_class, "approach-only");
            assert!(!gpl.hits.is_empty());
            for h in &gpl.hits {
                assert_eq!(
                    h.licence_warning,
                    "approach-only licence (GPL-3.0-only): read, do not copy"
                );
            }

            // Excluded: never served, by Def or by Ask, named or routed.
            for err in [
                client
                    .def(keyed(pb::HubDefRequest {
                        slug: "mini-closed".into(),
                        symbol: "CLOSED_LIMIT".into(),
                        k: 0,
                    }))
                    .await
                    .unwrap_err(),
                client
                    .ask(keyed(pb::HubAskRequest {
                        question: "CLOSED_LIMIT".into(),
                        corpus: "mini-closed".into(),
                        mode: String::new(),
                        max_tokens: 0,
                    }))
                    .await
                    .unwrap_err(),
            ] {
                assert_eq!(err.code(), tonic::Code::NotFound, "{err:?}");
                assert!(
                    !err.message().contains("closed"),
                    "a refusal names nothing: {err:?}"
                );
            }
            let routed = client
                .ask(keyed(pb::HubAskRequest {
                    question: "what is the value of CLOSED_LIMIT".into(),
                    corpus: String::new(),
                    mode: String::new(),
                    max_tokens: 0,
                }))
                .await;
            assert!(
                routed.is_err(),
                "an excluded corpus is never routed to: {routed:?}"
            );

            // Limits: the HTTP routes' own.
            let err = client
                .def(keyed(pb::HubDefRequest {
                    slug: "mini-c".into(),
                    symbol: "x".repeat(600),
                    k: 0,
                }))
                .await
                .unwrap_err();
            assert_eq!(err.code(), tonic::Code::InvalidArgument);
            let err = client
                .def(keyed(pb::HubDefRequest {
                    slug: "../etc".into(),
                    symbol: "MINI_LIMIT".into(),
                    k: 0,
                }))
                .await
                .unwrap_err();
            assert_eq!(err.code(), tonic::Code::NotFound);
            let err = client
                .ask(keyed(pb::HubAskRequest {
                    question: "q".into(),
                    corpus: String::new(),
                    mode: "semantic".into(),
                    max_tokens: 0,
                }))
                .await
                .unwrap_err();
            assert_eq!(err.code(), tonic::Code::InvalidArgument);

            // Ask, corpus omitted: routed by the sketch, answered in def mode.
            let ask = client
                .ask(keyed(pb::HubAskRequest {
                    question: "what is the value of MINI_LIMIT".into(),
                    corpus: String::new(),
                    mode: String::new(),
                    max_tokens: 0,
                }))
                .await
                .expect("Ask answers")
                .into_inner();
            assert_eq!(ask.corpus, "mini-c", "the corpus is always named");
            assert_eq!(ask.mode, "def");
            assert_eq!(ask.hits[0].value, "128");
            assert_eq!(ask.answered_from, "hosted");

            // Ask, named corpus, a how question: passages from the docs index.
            let how = client
                .ask(keyed(pb::HubAskRequest {
                    question: "frobnicate".into(),
                    corpus: "mini-c".into(),
                    mode: "how".into(),
                    max_tokens: 0,
                }))
                .await
                .expect("Ask how answers")
                .into_inner();
            assert_eq!(how.mode, "how");
            assert!(
                how.hits.iter().any(|h| h.file.contains("hash.c")),
                "{how:?}"
            );

            // Ask on an approach-only corpus: the flow's warning line as well.
            let ask_gpl = client
                .ask(keyed(pb::HubAskRequest {
                    question: "GPL_LIMIT".into(),
                    corpus: "mini-gpl".into(),
                    mode: "def".into(),
                    max_tokens: 0,
                }))
                .await
                .unwrap()
                .into_inner();
            assert!(
                ask_gpl
                    .warnings
                    .iter()
                    .any(|w| w.starts_with("approach-only licence (GPL-3.0-only)")),
                "{ask_gpl:?}"
            );
            assert!(ask_gpl.hits.iter().all(|h| !h.licence_warning.is_empty()));

            // Catalogue: the file's exact bytes and the decoded detached signature.
            let cat = client
                .catalogue(keyed(pb::HubCatalogueRequest {}))
                .await
                .unwrap()
                .into_inner();
            assert_eq!(cat.catalogue_json, std::fs::read(&fx.catalogue).unwrap());
            assert_eq!(cat.signature.len(), 64);
            assert_eq!(cat.signature[0], 1);
            assert_eq!(cat.signature[63], 0x40);

            // Nothing above wrote to the node: same indices, same counts.
            let (_, indices) = http(
                &app,
                "GET",
                "/_cat/indices/xc-*?format=json&h=index,docs.count",
                None,
            )
            .await;
            assert_eq!(indices.as_array().map(Vec::len), Some(6), "{indices}");

            let _ = tx.send(());
            let _ = server.await;
        }
    }
}
