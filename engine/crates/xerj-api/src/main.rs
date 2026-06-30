//! Standalone test binary for running just the xerj engine API
//! (native + ES-compat) without Xerj Console attached.  Useful for ES-compat
//! conformance testing and as the simplest possible invocation when
//! you don't want the full bundled UI.
//!
//! Production deployments use the `xerj` binary in xerj-server, which
//! merges this engine API with the bundled Xerj Console UI/API on the same
//! TCP listeners — see `engine/crates/xerj-server/src/main.rs`.

use xerj_api::{build_es_compat_router, build_native_router, AppState};
use xerj_common::{metrics::Metrics, Config};
use xerj_engine::Engine;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "xerj_api=info,tower_http=debug".into()),
        )
        .init();

    let config = Config::default();
    let metrics = Metrics::new()?;
    let engine = Engine::new(config.clone())?;
    let state = AppState::new(config.clone(), engine, metrics);

    let native_addr = config.rest_addr();
    let es_addr = config.es_compat_addr();

    tracing::info!("native API  → http://{native_addr}");
    tracing::info!("ES-compat   → http://{es_addr}");

    let native_router = build_native_router(state.clone());
    let es_router = build_es_compat_router(state);

    let native_listener = tokio::net::TcpListener::bind(&native_addr).await?;
    let es_listener = tokio::net::TcpListener::bind(&es_addr).await?;

    tokio::select! {
        result = axum::serve(native_listener, native_router) => { result?; }
        result = axum::serve(es_listener, es_router) => { result?; }
    }

    Ok(())
}
