//! # xerj-api
//!
//! REST API layer for the xerj search engine.
//!
//! Exposes **two** HTTP servers from the same binary:
//!
//! | Port | API | Description |
//! |------|-----|-------------|
//! | 8080 | Native | Clean 12-endpoint xerj API |
//! | 9200 | ES-compatible | Drop-in Elasticsearch 8.x replacement |
//!
//! ## Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────┐
//! │                   xerj-api                       │
//! │                                                   │
//! │  native.rs    ─── build_native_router(:8080)      │
//! │  es_compat.rs ─── build_es_compat_router(:9200)   │
//! │                          │                        │
//! │                    AppState (Arc-shared)           │
//! │                    ├─ Config                       │
//! │                    ├─ DashMap<name, IndexHandle>   │
//! │                    └─ Metrics                      │
//! └──────────────────────────────────────────────────┘
//! ```
//!
//! ## Quick start
//!
//! ```no_run
//! use xerj_api::{AppState, router::{build_native_router, build_es_compat_router}};
//! use xerj_common::{Config, metrics::Metrics};
//! use xerj_engine::Engine;
//!
//! #[tokio::main]
//! async fn main() {
//!     let config = Config::default();
//!     let engine = Engine::new(config.clone()).unwrap();
//!     let state = AppState::new(config, engine, Metrics::new().unwrap());
//!
//!     let native  = build_native_router(state.clone());
//!     let es      = build_es_compat_router(state);
//!
//!     let native_listener  = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
//!     let es_listener      = tokio::net::TcpListener::bind("0.0.0.0:9200").await.unwrap();
//!
//!     tokio::select! {
//!         _ = axum::serve(native_listener, native) => {}
//!         _ = axum::serve(es_listener, es)         => {}
//!     }
//! }
//! ```
//!
//! **Note:** the `xerj-api` binary above runs the engine API only.
//! The `xerj` binary in `xerj-server` adds the Xerj Console UI on the same
//! ports via `Router::merge`. Engine API and Xerj Console API are decoupled —
//! xerj-api itself does not depend on xerj-console-api.

pub mod auth;
pub mod binary_protocol;
pub mod error;
pub mod es_compat;
pub mod extract;
pub mod native;
pub mod responses;
pub mod router;
pub mod state;
pub mod stub;

// ── Convenience re-exports ────────────────────────────────────────────────────

pub use error::ApiError;
pub use router::{build_es_compat_router, build_native_router};
pub use state::{AppState, IndexHandle, IndexSettings};
