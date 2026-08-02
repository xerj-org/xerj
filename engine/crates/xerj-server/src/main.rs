//! # XERJ server entry point
//!
//! Parses CLI arguments, loads configuration, initialises observability,
//! auto-generates secrets and TLS credentials on first run, then starts
//! three concurrent TCP listeners:
//!
//! - Native REST  — default :8080
//! - ES-compat    — default :9200
//! - gRPC         — default :8081 (XerjSearch service, plaintext h2c)
//!
//! Shuts down gracefully on SIGTERM or SIGINT.
//!
//! ## Subcommands
//!
//! The binary also supports a `index` subcommand that ingests an NDJSON
//! file directly into the engine without going through HTTP.  This is the
//! fastest possible ingest path — it bypasses axum, hyper, tokio request
//! scheduling, and the bulk response serialiser entirely.
//!
//! ```text
//! xerj index --index <name> --file <path.ndjson> [--batch 5000] [--workers N]
//! ```

// ── Global allocator ─────────────────────────────────────────────────────────
// Use jemalloc instead of the system (glibc) malloc.  Under the heavy
// ingest-flush churn produced by log workloads, glibc malloc retains freed
// heap pages and never returns them to the OS, which causes RSS to grow
// monotonically until the cgroup OOM-kills the process.  jemalloc handles
// fragmentation actively and madvise()'s freed regions back.
#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

// jemalloc runtime config (read at process start via the `_rjem_malloc_conf`
// symbol — tikv-jemalloc is built with the `_rjem_` prefix, so plain
// `MALLOC_CONF` is IGNORED; only `_RJEM_MALLOC_CONF` or this symbol work).
//
// Why this is load-bearing: jemalloc's decay-based purging only runs inside
// allocator ticks and, without background threads, effectively never returns
// dirty pages under sustained multi-arena ingest churn.  Measured on a
// 62M-record bulk load: anon RSS grew ~4-5 KB per ingested doc (~15x the raw
// corpus, 83 GB at 15.5M docs) until the kernel OOM-killed the server, while
// live heap was a fraction of that.  `background_thread:true` +
// 1 s dirty/muzzy decay purges freed pages continuously; the same load then
// holds a flat working-set RSS.  Ingest throughput cost measured: none
// (within noise, ~36k docs/s single-index bulk either way).
#[cfg(all(
    not(target_env = "msvc"),
    not(feature = "debug-profiling"),
    target_os = "linux"
))]
#[allow(non_upper_case_globals)]
#[export_name = "_rjem_malloc_conf"]
pub static malloc_conf: &[u8] = b"background_thread:true,dirty_decay_ms:1000,muzzy_decay_ms:1000\0";

// macOS (and other non-Linux unix) jemalloc has no pthread background_thread
// support, so `background_thread:true` only prints `<jemalloc>: option
// background_thread currently supports pthread only` on every launch (reported
// by users running `xerj autoindex` on macOS). Keep the aggressive decay policy
// — it still purges freed pages on allocator ticks — without the unsupported knob.
#[cfg(all(
    not(target_env = "msvc"),
    not(feature = "debug-profiling"),
    not(target_os = "linux")
))]
#[allow(non_upper_case_globals)]
#[export_name = "_rjem_malloc_conf"]
pub static malloc_conf: &[u8] = b"dirty_decay_ms:1000,muzzy_decay_ms:1000\0";

// Preserve the production decay policy. Merely compiling this feature enables
// jemalloc's profiler; sampling remains inactive until the local controller
// starts a bounded capture.
#[cfg(all(not(target_env = "msvc"), feature = "debug-profiling"))]
#[allow(non_upper_case_globals)]
#[export_name = "_rjem_malloc_conf"]
pub static malloc_conf: &[u8] = b"background_thread:true,dirty_decay_ms:1000,muzzy_decay_ms:1000,prof:true,prof_active:false,lg_prof_sample:19\0";

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use axum::Router;
use axum_server::tls_rustls::RustlsConfig;
use tokio::net::TcpListener;
use tokio::signal;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use xerj_api::{build_es_compat_router, build_native_router, AppState};
use xerj_cluster::{transport::TcpTransport, ClusterNode, ClusterRunner};
use xerj_common::{config::Config, metrics::Metrics};
use xerj_console_api::{state::ClusterMode, ConsoleState};
use xerj_engine::Engine;

mod brain;
#[cfg(all(target_os = "linux", feature = "debug-profiling"))]
mod debug_profiling;
mod grpc;
mod ingest_memory_trace;

// ─────────────────────────────────────────────────────────────────────────────
// CLI
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct CliArgs {
    config: Option<PathBuf>,
    data_dir: Option<String>,
    insecure: bool,
    embed_mode: Option<String>,
    onnx_model: Option<String>,
    onnx_tokenizer: Option<String>,
    compat_distribution: Option<String>,
    compat_version: Option<String>,
}

fn parse_args() -> CliArgs {
    let mut args = std::env::args().skip(1);
    let mut config = None;
    let mut data_dir = None;
    let mut insecure = false;
    let mut embed_mode = None;
    let mut onnx_model = None;
    let mut onnx_tokenizer = None;
    let mut compat_distribution = None;
    let mut compat_version = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" | "-c" => {
                config = args.next().map(PathBuf::from);
            }
            "--data-dir" | "-d" => {
                data_dir = args.next();
            }
            "--insecure" | "-k" => {
                insecure = true;
            }
            "--embed-mode" => {
                embed_mode = args.next();
            }
            "--onnx-model" => onnx_model = args.next(),
            "--onnx-tokenizer" => onnx_tokenizer = args.next(),
            "--compat-distribution" => compat_distribution = args.next(),
            "--compat-version" => compat_version = args.next(),
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--version" | "-V" => {
                println!("xerj v{}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument: {other}. Use --help for usage.");
                std::process::exit(1);
            }
        }
    }

    CliArgs {
        config,
        data_dir,
        insecure,
        embed_mode,
        onnx_model,
        onnx_tokenizer,
        compat_distribution,
        compat_version,
    }
}

fn print_help() {
    println!(
        "xerj v{} — the unified search engine for AI (Elasticsearch wire-compatible)\n\
         \n\
         USAGE:\n\
             xerj [OPTIONS]\n\
         \n\
         OPTIONS:\n\
             --config,   -c <PATH>  Path to TOML config file\n\
             --data-dir, -d <PATH>  Override data directory\n\
             --insecure, -k         Disable TLS\n\
             --embed-mode <MODE>    Embedding backend: lexical | neural | proxy | auto |\n\
                                      onnx-experimental\n\
                                      lexical  built-in feature-hash (default; offline, not neural)\n\
                                      neural   built-in BERT (all-MiniLM-L6-v2) — real semantics,\n\
                                               in-process; auto-downloads the model (~90 MB) on\n\
                                               first use, then runs from cache. Just add the flag.\n\
                                      proxy    external OpenAI-compatible /v1/embeddings endpoint\n\
                                      auto     proxy if embedding.default_endpoint is set, else lexical\n\
                                      onnx-experimental  local all-MiniLM-L6-v2-compatible FP32\n\
                                               ONNX prototype; width 384, named BERT inputs and\n\
                                               token output; requires matching tokenizer.json;\n\
                                               no auto-download\n\
             --onnx-model <PATH>    compatible FP32 MiniLM ONNX model\n\
             --onnx-tokenizer <PATH> tokenizer.json from the same model/export\n\
             --compat-distribution <DIST>  Force the wire-compat client identity: elasticsearch |\n\
                                      opensearch. Default: unset — xerj auto-detects per request\n\
                                      from the caller's User-Agent header instead (an\n\
                                      opensearch-py/opensearch-js client — including OpenSearch\n\
                                      Dashboards itself — and an elasticsearch-py/elasticsearch-js\n\
                                      client hitting the SAME running instance each see GET / and\n\
                                      GET /_nodes shaped for their own ecosystem). Setting this\n\
                                      pins ALL clients to one identity regardless of User-Agent —\n\
                                      useful if a client sends no recognizable User-Agent, or you\n\
                                      just want deterministic behavior.\n\
                                      Example: --compat-distribution opensearch\n\
             --compat-version <VER> Force the reported version.number (e.g. 2.11.0). Default:\n\
                                      unset — when auto-detected as OpenSearch, reports a fixed\n\
                                      version verified to pass OpenSearch Dashboards' own\n\
                                      compatibility check (not the calling client's own version —\n\
                                      client library versions aren't a reliable stand-in for a\n\
                                      compatible server version, confirmed against a real OSD\n\
                                      container). Example: --compat-version 2.11.0\n\
             --help,     -h         Show this help\n\
             --version,  -V         Print version and exit\n\
         \n\
         SUBCOMMANDS:\n\
             xerj index      <opts>          direct NDJSON → engine ingest (see xerj index --help)\n\
             xerj autoindex  <folder> [opts] zero-config folder discovery + indexing (see xerj autoindex --help)\n\
             xerj autoindex  map             print the discovered data map\n\
             xerj brain      <folder>        one command: index a folder into a running, browsable\n\
                                             second brain in your browser (see xerj brain --help)\n\
         \n\
         ENVIRONMENT:\n\
             XERJ_LOG         Log level filter (default: info)\n\
             XERJ_CONFIG      Config file path\n\
             XERJ_EMBED_MODE  Embedding backend (lexical|neural|proxy|auto|onnx-experimental)\n\
             XERJ_ONNX_MODEL  FP32 ONNX model path\n\
             XERJ_ONNX_TOKENIZER matching tokenizer.json path\n\
             XERJ_COMPAT_DISTRIBUTION  elasticsearch|opensearch — same as --compat-distribution\n\
             XERJ_COMPAT_VERSION       same as --compat-version\n\
             XERJ_INGEST_MEMORY_TRACE  off|summary (default: off); summary emits bounded\n\
                                      periodic NDJSON diagnostics, never per-item events\n\
             XERJ_INGEST_MEMORY_SAMPLE_MS  sampler period, clamped to 25..60000 ms\n\
             XERJ_INGEST_MEMORY_OUTPUT tracing|<PATH> (default: tracing); append NDJSON to path\n\
                                      Merge/cache fields are reserved/unavailable in v1.\n\
                                      Active+drained may transiently overlap across sampling.\n\
         \n\
         WIRE-COMPAT IDENTITY — three ways to run this, pick what fits:\n\
         \n\
             1. Fully automatic (default, no flags):\n\
                  xerj -d ./data\n\
                Every client gets a response shaped for its own ecosystem, detected from its\n\
                User-Agent on every request. One instance serves ES and OpenSearch clients at\n\
                once.\n\
         \n\
             2. Force one identity for every client, every request:\n\
                  xerj -d ./data --compat-distribution opensearch --compat-version 2.11.0\n\
                Or via env (handy in docker-compose):\n\
                  XERJ_COMPAT_DISTRIBUTION=opensearch XERJ_COMPAT_VERSION=2.11.0 xerj -d ./data\n\
                Ignores User-Agent entirely — useful for a client that doesn't send one, or when\n\
                you want fully deterministic behavior.\n\
         \n\
             3. Pin only the distribution, let the version auto-detect per client:\n\
                  xerj -d ./data --compat-distribution opensearch\n\
                (or set only XERJ_COMPAT_VERSION and leave distribution to auto-detect — the two\n\
                settings are resolved independently, mix and match as needed.)\n",
        env!("CARGO_PKG_VERSION")
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Startup banner
// ─────────────────────────────────────────────────────────────────────────────

fn print_banner(cfg: &Config, startup_ms: u128) {
    let tls = if cfg.tls.enabled { "TLS " } else { "plain" };
    println!();
    println!("  ██╗  ██╗███████╗██████╗      ██╗");
    println!("  ╚██╗██╔╝██╔════╝██╔══██╗     ██║");
    println!("   ╚███╔╝ █████╗  ██████╔╝     ██║");
    println!("   ██╔██╗ ██╔══╝  ██╔══██╗██   ██║");
    println!("  ██╔╝ ██╗███████╗██║  ██║╚█████╔╝");
    println!(
        "  ╚═╝  ╚═╝╚══════╝╚═╝  ╚═╝ ╚════╝   v{}",
        env!("CARGO_PKG_VERSION")
    );
    println!();
    println!(" the unified search engine for AI — connect, autoindex, query");
    println!();
    println!(" Native REST  :{} [{}]", cfg.server.rest_port, tls);
    println!(" ES-compat    :{} [{}]", cfg.server.es_compat_port, tls);
    println!(" gRPC         :{} [h2c]", cfg.server.grpc_port);
    println!(" Data dir     {}", cfg.server.data_dir);
    println!(" Started in   {}ms", startup_ms);
    println!();
    println!(
        " Xerj Console UI    http://localhost:{}/_xerj-console/  ({} files bundled)",
        cfg.server.es_compat_port,
        xerj_console_api::spa::asset_count(),
    );
    println!();

    // ── Honesty banner: surface deployment-security expectations ──────────
    //
    // The 2026-04-25 fairness review found the brief implies more
    // engine-level security than ships today. Print the actual posture so
    // an operator sees it on every start. Suppress nothing — these lines
    // map 1:1 to items on the path-to-100% plan.
    println!(" ┌─ Deployment posture (see PATH_TO_100_PCT_v0.6.0_to_v1.0.md) ──");
    if !cfg.tls.enabled {
        println!(" │ ⚠  TLS:    listener is plain TCP — terminate TLS at a reverse proxy");
        println!(" │           (or enable in-process TLS: tls.enabled = true)");
    } else {
        println!(" │ ✓  TLS:    in-process rustls termination active (REST + ES-compat)");
        println!(" │           (self-signed by default — supply a CA cert for production)");
    }
    if !cfg.auth.enabled {
        println!(" │ ⚠  Auth:   DISABLED (--insecure) — anyone on the network can write");
    } else {
        println!(
            " │ ✓  Auth:   single API-key (no RBAC; per-doc / per-field controls roadmap v0.9)"
        );
    }
    println!(" │ ⚠  Audit:  request tracing only — tamper-evident WORM audit log v0.9");
    println!(" │ ⚠  Encryption-at-rest: not engine-level — use OS FDE or S3 SSE for now");
    println!(" └────────────────────────────────────────────────────────────────");
    println!();
}

// ─────────────────────────────────────────────────────────────────────────────
// Config loading
// ─────────────────────────────────────────────────────────────────────────────

fn load_config(args: &CliArgs) -> Result<Config> {
    let config_path = args
        .config
        .clone()
        .or_else(|| std::env::var("XERJ_CONFIG").ok().map(PathBuf::from));

    let mut cfg = if let Some(path) = config_path {
        info!("loading config from {}", path.display());
        Config::load(&path).with_context(|| format!("load config from {}", path.display()))?
    } else {
        info!("no config file — using defaults");
        Config::default()
    };

    if let Some(dir) = &args.data_dir {
        cfg.server.data_dir = dir.clone();
    }

    if args.insecure {
        warn!("--insecure: TLS and auth disabled");
        cfg.tls.enabled = false;
        cfg.auth.enabled = false;
    }

    // Embedding backend override: `--embed-mode` flag or `XERJ_EMBED_MODE`
    // env (flag wins). Also accepts `onnx-experimental` in a feature-enabled
    // build with explicit model and tokenizer paths.
    if let Some(mode) = args
        .embed_mode
        .clone()
        .or_else(|| std::env::var("XERJ_EMBED_MODE").ok())
    {
        let mode = mode.trim().to_ascii_lowercase();
        match mode.as_str() {
            "lexical" | "neural" | "proxy" | "auto" | "onnx-experimental" => {
                info!("embedding.mode = {mode} (from CLI/env)");
                cfg.embedding.mode = mode;
            }
            other => {
                anyhow::bail!("unknown --embed-mode '{other}'; use lexical|neural|proxy|auto|onnx-experimental");
            }
        }
    }
    if let Some(path) = args
        .onnx_model
        .clone()
        .or_else(|| std::env::var("XERJ_ONNX_MODEL").ok())
    {
        cfg.embedding.onnx_model_path = path;
    }
    if let Some(path) = args
        .onnx_tokenizer
        .clone()
        .or_else(|| std::env::var("XERJ_ONNX_TOKENIZER").ok())
    {
        cfg.embedding.onnx_tokenizer_path = path;
    }
    if cfg.embedding.mode == "onnx-experimental" {
        #[cfg(not(feature = "onnx-experimental"))]
        anyhow::bail!(
            "onnx-experimental was requested, but this binary was built without it; \
             rebuild with `cargo build --release -p xerj-server --features onnx-experimental`"
        );
        #[cfg(feature = "onnx-experimental")]
        {
            let model = Path::new(&cfg.embedding.onnx_model_path);
            let tokenizer = Path::new(&cfg.embedding.onnx_tokenizer_path);
            if !model.is_file() {
                anyhow::bail!(
                    "onnx-experimental needs a readable FP32 model file at {:?}; \
                     pass --onnx-model <PATH> (no model is downloaded automatically)",
                    cfg.embedding.onnx_model_path
                );
            }
            if !tokenizer.is_file() {
                anyhow::bail!(
                    "onnx-experimental needs the matching tokenizer.json at {:?}; \
                     pass --onnx-tokenizer <PATH>",
                    cfg.embedding.onnx_tokenizer_path
                );
            }
            if cfg.embedding.onnx_max_inflight_calls == 0
                || cfg.embedding.onnx_max_input_bytes_per_call == 0
                || cfg.embedding.onnx_max_inflight_input_bytes == 0
                || cfg.embedding.onnx_max_input_bytes_per_call
                    > cfg.embedding.onnx_max_inflight_input_bytes
                || cfg.embedding.onnx_max_inflight_input_bytes > u32::MAX as usize
            {
                anyhow::bail!(
                    "invalid ONNX admission limits: inflight_calls={}, bytes_per_call={}, \
                     inflight_bytes={}; use non-zero values, keep bytes_per_call <= \
                     inflight_bytes, and inflight_bytes <= {}",
                    cfg.embedding.onnx_max_inflight_calls,
                    cfg.embedding.onnx_max_input_bytes_per_call,
                    cfg.embedding.onnx_max_inflight_input_bytes,
                    u32::MAX
                );
            }
        }
    }

    // ES/OpenSearch wire-compatibility override: `--compat-distribution` /
    // `XERJ_COMPAT_DISTRIBUTION` (flag wins). Left unset, xerj auto-detects
    // per request from the caller's User-Agent instead — this is only for
    // pinning deterministic behavior (or covering a client whose
    // User-Agent isn't recognized).
    if let Some(dist) = args
        .compat_distribution
        .clone()
        .or_else(|| std::env::var("XERJ_COMPAT_DISTRIBUTION").ok())
    {
        let dist = dist.trim().to_ascii_lowercase();
        match dist.as_str() {
            "elasticsearch" | "opensearch" => {
                info!("compat.distribution = {dist} (forced via CLI/env)");
                cfg.compat.distribution = dist;
            }
            other => {
                anyhow::bail!(
                    "unknown --compat-distribution '{other}'; use elasticsearch|opensearch"
                );
            }
        }
    }
    if let Some(version) = args
        .compat_version
        .clone()
        .or_else(|| std::env::var("XERJ_COMPAT_VERSION").ok())
    {
        info!("compat.version = {version} (forced via CLI/env)");
        cfg.compat.version = version;
    }

    Ok(cfg)
}

// ─────────────────────────────────────────────────────────────────────────────
// Admin API key
// ─────────────────────────────────────────────────────────────────────────────

/// Write a secret to `path`, readable by the owner only (0600) — the same
/// posture as the Xerj Console master key (`bootstrap.rs`,
/// `set_owner_readable_only`).
///
/// RC4 W2 #21: the admin API key and the TLS private key were written with
/// `std::fs::write`, which creates files as `0666 & !umask` = 0664 on stock
/// Linux — group/world-readable secrets. On unix the file is now *created*
/// 0600 (`OpenOptions::mode`), so there is no window where another user can
/// open it; the explicit `set_permissions` afterwards tightens files that
/// already exist from an earlier version (a create mode never applies to a
/// pre-existing file).
fn write_secret_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = f.metadata()?.permissions();
        perm.set_mode(0o600);
        f.set_permissions(perm)?;
    }
    Ok(())
}

fn ensure_admin_key(cfg: &mut Config) -> Result<()> {
    if !cfg.auth.enabled || !cfg.auth.admin_api_key.is_empty() {
        return Ok(());
    }

    let data_dir = Path::new(&cfg.server.data_dir);
    let key_path = data_dir.join("admin.key");

    // A prior run may have already persisted a key — reuse it instead of
    // minting a fresh one on every restart. A regenerated key on each
    // restart forces every client (Kibana/OSD, MCP configs, scripts) to be
    // manually re-pointed at the new value, which is exactly what
    // persisting to `admin.key` in the first place was meant to avoid.
    // Only a well-formed 64-char hex string (the shape this function itself
    // always produces) is trusted; anything else falls through to a fresh
    // generation rather than risk starting up with a garbled credential.
    if let Ok(existing) = std::fs::read_to_string(&key_path) {
        let existing = existing.trim();
        if existing.len() == 64 && existing.bytes().all(|b| b.is_ascii_hexdigit()) {
            cfg.auth.admin_api_key = existing.to_string();
            return Ok(());
        }
    }

    // Generate 32 random bytes → 64-char hex string
    let raw: Vec<u8> = (0..32).map(|_| rand::random::<u8>()).collect();
    let key: String = raw.iter().map(|b| format!("{b:02x}")).collect();

    println!();
    println!("╔══════════════════════════════════════════════════╗");
    println!("║  First-run: admin API key auto-generated         ║");
    println!("║                                                  ║");
    println!("║  {:<48} ║", key);
    println!("║                                                  ║");
    println!("║  Keep this secret. Written to:                   ║");
    println!("║  {:<48} ║", format!("{}/admin.key", cfg.server.data_dir));
    println!("╚══════════════════════════════════════════════════╝");
    println!();

    if std::fs::create_dir_all(data_dir).is_ok() {
        // 0600 — this file IS the admin credential (RC4 W2 #21).
        if let Err(e) = write_secret_file(&key_path, key.as_bytes()) {
            warn!("could not persist admin.key: {e}");
        }
    }

    cfg.auth.admin_api_key = key;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// TLS auto-generation
// ─────────────────────────────────────────────────────────────────────────────

fn ensure_tls_cert(cfg: &mut Config) -> Result<()> {
    if !cfg.tls.enabled {
        return Ok(());
    }

    let cert_exists = !cfg.tls.cert_path.is_empty() && Path::new(&cfg.tls.cert_path).exists();
    let key_exists = !cfg.tls.key_path.is_empty() && Path::new(&cfg.tls.key_path).exists();

    if cert_exists && key_exists {
        info!(
            "using TLS cert from {} / {}",
            cfg.tls.cert_path, cfg.tls.key_path
        );
        // RC4 W2 #21 (legacy remediation): versions before this fix wrote the
        // auto-generated key 0664, and this reuse path never rewrites the
        // file — without a fixup here an old deployment keeps its
        // world-readable TLS key forever. Tighten it to 0600 *only* when it
        // is OUR auto-generated key (`<data_dir>/xerj.key`); an
        // operator-supplied key may be group-readable by design (e.g. an
        // ssl-cert group shared across services), so warn without touching
        // their permissions.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let key_path = Path::new(&cfg.tls.key_path);
            if let Ok(meta) = std::fs::metadata(key_path) {
                let mode = meta.permissions().mode() & 0o777;
                if mode & 0o077 != 0 {
                    let autogen = Path::new(&cfg.server.data_dir).join("xerj.key");
                    // Canonicalize both sides so `./data/xerj.key` and an
                    // absolute spelling of the same file compare equal.
                    let is_autogen = key_path
                        .canonicalize()
                        .ok()
                        .zip(autogen.canonicalize().ok())
                        .map(|(a, b)| a == b)
                        .unwrap_or(false);
                    if is_autogen {
                        let mut perm = meta.permissions();
                        perm.set_mode(0o600);
                        match std::fs::set_permissions(key_path, perm) {
                            Ok(()) => {
                                info!("tightened {} to 0600 (was {mode:o})", cfg.tls.key_path)
                            }
                            Err(e) => warn!(
                                "could not tighten {} (mode {mode:o}) to 0600: {e}",
                                cfg.tls.key_path
                            ),
                        }
                    } else {
                        warn!(
                            "TLS private key {} is group/world-readable (mode {mode:o}) — consider chmod 0600",
                            cfg.tls.key_path
                        );
                    }
                }
            }
        }
        return Ok(());
    }

    info!("generating self-signed TLS certificate for localhost");

    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])
        .context("generate self-signed cert")?;

    let data_dir = Path::new(&cfg.server.data_dir);
    std::fs::create_dir_all(data_dir).context("create data dir for TLS cert")?;

    let cert_path = data_dir.join("xerj.crt");
    let key_path = data_dir.join("xerj.key");

    // The certificate is public material — default perms are fine.  The
    // private key is a secret: created 0600 (RC4 W2 #21).
    std::fs::write(&cert_path, cert.cert.pem())
        .with_context(|| format!("write cert to {}", cert_path.display()))?;
    write_secret_file(&key_path, cert.key_pair.serialize_pem().as_bytes())
        .with_context(|| format!("write key to {}", key_path.display()))?;

    cfg.tls.cert_path = cert_path.to_string_lossy().into_owned();
    cfg.tls.key_path = key_path.to_string_lossy().into_owned();

    warn!("self-signed certificate generated — replace with a real cert for production");
    Ok(())
}

/// Build the in-process TLS server config from the PEM cert/key that
/// `ensure_tls_cert` produced.  Returns `None` when TLS is disabled.
///
/// Installs the `ring` rustls crypto provider as the process default —
/// rustls 0.23's infallible `ServerConfig::builder()` (used inside
/// axum-server's `from_pem_file`) panics without one.  `install_default`
/// is idempotent-safe: a redundant call returns `Err`, which we ignore.
///
/// If TLS is enabled but the cert/key fail to load we return the error
/// (fail loud) rather than silently downgrading the operator-requested
/// HTTPS listener to cleartext.
async fn build_tls_config(cfg: &Config) -> Result<Option<RustlsConfig>> {
    if !cfg.tls.enabled {
        return Ok(None);
    }

    let _ = rustls::crypto::ring::default_provider().install_default();

    let config = RustlsConfig::from_pem_file(&cfg.tls.cert_path, &cfg.tls.key_path)
        .await
        .with_context(|| {
            format!(
                "load TLS cert {} / key {} (TLS is enabled)",
                cfg.tls.cert_path, cfg.tls.key_path
            )
        })?;

    Ok(Some(config))
}

// ─────────────────────────────────────────────────────────────────────────────
// Observability
// ─────────────────────────────────────────────────────────────────────────────

fn init_tracing(logging: &xerj_common::config::LoggingConfig) {
    let mut filter = EnvFilter::try_from_env("XERJ_LOG")
        .or_else(|_| EnvFilter::try_from_env("RUST_LOG"))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let onnx_log = std::env::var("XERJ_ONNX_LOG")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(onnx_log.as_str(), "info" | "debug" | "verbose" | "trace") {
        // `ort`'s tracing bridge emits allocator and graph-optimizer internals
        // at INFO independently of the ONNX Runtime session severity. Keep
        // normal XERJ logs concise; XERJ_ONNX_LOG explicitly opts back in.
        filter = filter.add_directive("ort=warn".parse().expect("valid ort log directive"));
    }

    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_thread_ids(false);

    // RC4-W4 item 6: JSON structured logs for shippers, or compact text
    // (default). Both branches call `.init()` so the differing builder types
    // never need to unify.
    if logging.is_json() {
        builder.json().init();
    } else {
        builder.compact().init();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Graceful shutdown
// ─────────────────────────────────────────────────────────────────────────────

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let sigterm = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let sigterm = std::future::pending::<()>();

    // `shutdown_signal()` is awaited once per listener (native REST, ES-compat,
    // gRPC), so all of them wake on the same Ctrl-C. Log the banner only once.
    static SHUTDOWN_LOGGED: std::sync::Once = std::sync::Once::new();
    tokio::select! {
        _ = ctrl_c  => { SHUTDOWN_LOGGED.call_once(|| info!("SIGINT received — shutting down")); }
        _ = sigterm => { SHUTDOWN_LOGGED.call_once(|| info!("SIGTERM received — shutting down")); }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Periodic background flush
// ─────────────────────────────────────────────────────────────────────────────

/// Spawn the periodic background flush task: every `every`, flush any index
/// whose memtable is over its configured thresholds
/// (`Engine::flush_all_if_needed`).
///
/// This is the safety net for memtable state the write-path flush scheduler
/// (`maybe_spawn_flush`, which only runs inside write requests) cannot see:
///
/// - WAL replay at boot can leave an index over-threshold before any write
///   arrives;
/// - a write-path flush skipped under flush-permit exhaustion is never
///   retried if the writes stop.
///
/// Runs until aborted (the caller aborts it after the listeners exit, right
/// before the final `flush_all_force`). `MissedTickBehavior::Delay` keeps a
/// flush that overruns the interval from being punished with an immediate
/// burst of catch-up ticks.
fn spawn_periodic_flusher(
    engine: std::sync::Arc<Engine>,
    every: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(every);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval.tick().await; // consume the immediate first tick
        loop {
            interval.tick().await;
            engine.flush_all_if_needed().await;
        }
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Server runners
// ─────────────────────────────────────────────────────────────────────────────

/// Serve `router` on `addr`, plain or TLS.
///
/// Both paths install `ConnectInfo<SocketAddr>` via
/// `into_make_service_with_connect_info`, so handlers can reach the real TCP
/// peer. That is what the console's per-IP auth rate limiter keys on — it used
/// to key on `x-forwarded-for`, which the caller writes, so a rotating header
/// bought an unlimited quota (#76 S5-4). Forwarding headers are believed only
/// when the peer is listed in `server.trusted_proxies`; without connect-info on
/// *both* listeners the limiter would fall back to a single shared bucket.
async fn serve(
    router: Router,
    addr: SocketAddr,
    name: &'static str,
    tls: Option<RustlsConfig>,
) -> Result<()> {
    // ── TLS path: in-process rustls termination via axum-server ──────────
    //
    // When TLS is enabled the plain `axum::serve` (hyper) accept loop can't
    // be used — it speaks cleartext.  `axum_server::bind_rustls` wraps every
    // accepted TCP connection in a `tokio_rustls::TlsAcceptor` handshake
    // before handing the decrypted stream to the same axum `Router`.  The
    // ServerConfig (cert chain + key + ALPN h2/http1) is built once by the
    // caller from the PEM files that `ensure_tls_cert` generated/validated.
    if let Some(tls) = tls {
        // axum-server drives shutdown through a `Handle` rather than a
        // graceful-shutdown future, so bridge our SIGINT/SIGTERM signal to
        // it: on signal, stop accepting and drain in-flight connections
        // (10 s grace) so ongoing requests finish cleanly.
        let handle = axum_server::Handle::new();
        let shutdown_handle = handle.clone();
        tokio::spawn(async move {
            shutdown_signal().await;
            shutdown_handle.graceful_shutdown(Some(std::time::Duration::from_secs(10)));
        });

        info!("{name} listening on {addr} (TLS)");

        axum_server::bind_rustls(addr, tls)
            .handle(handle)
            .serve(router.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .with_context(|| format!("{name} serve error (TLS)"))?;

        info!("{name} shut down cleanly");
        return Ok(());
    }

    // ── Plain path: unchanged cleartext HTTP ─────────────────────────────
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("{name}: bind {addr}"))?;

    info!("{name} listening on {addr}");

    // TCP_NODELAY: disable Nagle on accepted connections.  Elasticsearch
    // (Netty) sets this by default; without it small request/response
    // round-trips can stall on the delayed-ACK/Nagle interaction, which
    // shows up as a fixed per-request latency tax on trivial reads.
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .tcp_nodelay(true)
    .with_graceful_shutdown(shutdown_signal())
    .await
    .with_context(|| format!("{name} serve error"))?;

    info!("{name} shut down cleanly");
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Main
// ─────────────────────────────────────────────────────────────────────────────

// ─────────────────────────────────────────────────────────────────────────────
// CLI `index` subcommand — direct file → engine ingest (bypasses HTTP)
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct IndexCmdArgs {
    index: String,
    file: PathBuf,
    batch: usize,
    workers: usize,
    limit: usize,
    config: Option<PathBuf>,
    data_dir: Option<String>,
}

fn parse_index_args() -> IndexCmdArgs {
    // Skip argv[0] (binary) + argv[1] ("index").
    let mut args = std::env::args().skip(2);
    let mut index = None;
    let mut file = None;
    let mut batch = 5000usize;
    let mut workers = 0usize; // 0 = num_cpus
    let mut limit = 0usize; // 0 = all lines
    let mut config = None;
    let mut data_dir = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--index" | "-i" => index = args.next(),
            "--file" | "-f" => file = args.next().map(PathBuf::from),
            "--batch" | "-b" => {
                batch = args.next().and_then(|s| s.parse().ok()).unwrap_or(5000);
            }
            "--workers" | "-w" => {
                workers = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            }
            "--limit" | "-l" => {
                limit = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            }
            "--config" | "-c" => config = args.next().map(PathBuf::from),
            "--data-dir" | "-d" => data_dir = args.next(),
            "--help" | "-h" => {
                print_index_help();
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument: {other}. Use --help for usage.");
                std::process::exit(1);
            }
        }
    }

    let index = index.unwrap_or_else(|| {
        eprintln!("error: --index <name> is required");
        std::process::exit(2);
    });
    let file = file.unwrap_or_else(|| {
        eprintln!("error: --file <path> is required");
        std::process::exit(2);
    });

    IndexCmdArgs {
        index,
        file,
        batch,
        workers,
        limit,
        config,
        data_dir,
    }
}

fn print_index_help() {
    println!(
        "xerj index — ingest an NDJSON file directly into the engine\n\
         \n\
         USAGE:\n\
             xerj index --index <name> --file <ndjson> [OPTIONS]\n\
         \n\
         OPTIONS:\n\
             --index,    -i <NAME>  Target index name (required)\n\
             --file,     -f <PATH>  Path to NDJSON file (required)\n\
             --batch,    -b <N>     Docs per batch (default 5000)\n\
             --workers,  -w <N>     Parallel ingest workers (default = num_cpus)\n\
             --limit,    -l <N>     Ingest only the first N lines (default: all)\n\
             --config,   -c <PATH>  Path to TOML config file\n\
             --data-dir, -d <PATH>  Override data directory\n\
             --help,     -h         Show this help\n\
         \n\
         Bypasses HTTP/axum entirely. Bytes are pushed straight into\n\
         index_batch_turbo_raw via rayon workers. Use this for maximum\n\
         single-node ingest throughput — it's the xerj equivalent of\n\
         Lucene's IndexWriter fed from a file."
    );
}

/// Run the `xerj index` subcommand — direct NDJSON → engine ingest.
///
/// Memory-maps the file, finds newline boundaries in parallel via rayon,
/// chunks into batches of `batch` complete NDJSON lines, then dispatches
/// batches concurrently to `index_batch_turbo_raw`.  Zero HTTP overhead,
/// zero axum/hyper, zero per-item response JSON serialisation, and zero
/// file-reader thread contention — the whole file is `&[u8]` in memory and
/// all 32 cores can scan chunks of it in parallel.
async fn run_cli_index(cmd: IndexCmdArgs) -> Result<()> {
    use std::sync::Arc as StdArc;
    use tokio::sync::Semaphore;

    // Load config but override to a minimal in-process shape.
    let fake_cli = CliArgs {
        config: cmd.config.clone(),
        data_dir: cmd.data_dir.clone(),
        insecure: true,
        embed_mode: None,
        onnx_model: None,
        onnx_tokenizer: None,
        compat_distribution: None,
        compat_version: None,
    };
    let mut cfg = load_config(&fake_cli)?;
    // Tracing after config so the [logging] format applies (RC4-W4 item 6).
    init_tracing(&cfg.logging);
    cfg.tls.enabled = false;
    cfg.auth.enabled = false;
    cfg.cluster.enabled = false;

    std::fs::create_dir_all(&cfg.server.data_dir)
        .with_context(|| format!("create data dir {}", cfg.server.data_dir))?;

    info!(
        index = cmd.index.as_str(),
        file = %cmd.file.display(),
        batch = cmd.batch,
        workers = cmd.workers,
        "xerj CLI index: starting"
    );

    // Open the engine.  This replays any WAL and opens existing segments.
    let engine = Engine::new(cfg.clone()).context("initialise engine")?;
    let index = engine
        .get_or_create_index(&cmd.index)
        .with_context(|| format!("get_or_create_index({})", cmd.index))?;

    let batch_size = cmd.batch.max(1);
    let workers = if cmd.workers == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(8)
    } else {
        cmd.workers
    };

    // Semaphore bounds in-flight ingest work so we don't balloon memory
    // with pre-built batches on a slow engine.
    let sem = StdArc::new(Semaphore::new(workers * 2));

    // M5.14 — memory-map the NDJSON file and let rayon scan chunks of
    // it in parallel to find newline boundaries.  This replaces the
    // pre-M5.14 BufReader::lines() loop that was pinned to one thread
    // at ~400 MB/s and became the hot-path bottleneck once xerj was
    // doing 900k docs/s peak.
    let file =
        std::fs::File::open(&cmd.file).with_context(|| format!("open {}", cmd.file.display()))?;
    let file_size = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mmap: memmap2::Mmap = unsafe { memmap2::Mmap::map(&file) }
        .with_context(|| format!("mmap {}", cmd.file.display()))?;
    // Advise the kernel that we'll read sequentially and that we plan
    // to need the whole thing — lets it pre-fetch aggressively.
    #[cfg(target_os = "linux")]
    {
        let _ = mmap.advise(memmap2::Advice::Sequential);
        let _ = mmap.advise(memmap2::Advice::WillNeed);
    }
    let data: &'static [u8] = unsafe {
        // SAFETY: we keep `mmap` alive for the rest of this function
        // so the 'static promise holds for the duration of the batches
        // we dispatch from it.  We join all JoinHandles before dropping
        // the mmap.
        std::slice::from_raw_parts(mmap.as_ptr(), mmap.len())
    };

    let start = std::time::Instant::now();
    let total_sent = StdArc::new(std::sync::atomic::AtomicU64::new(0));
    let total_errs = StdArc::new(std::sync::atomic::AtomicU64::new(0));

    // Progress reporter — prints every 5 s.
    let report_sent = StdArc::clone(&total_sent);
    let report_errs = StdArc::clone(&total_errs);
    let reporter = tokio::spawn(async move {
        let t0 = std::time::Instant::now();
        let mut last_sent = 0u64;
        let mut last_t = t0;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let now = std::time::Instant::now();
            let sent = report_sent.load(std::sync::atomic::Ordering::Relaxed);
            let errs = report_errs.load(std::sync::atomic::Ordering::Relaxed);
            let win_rate = (sent - last_sent) as f64 / (now - last_t).as_secs_f64();
            let avg_rate = sent as f64 / (now - t0).as_secs_f64();
            eprintln!(
                "[{:6.1}s] sent={sent:>12} errs={errs} win_rate={win_rate:>10.0}/s avg_rate={avg_rate:>10.0}/s",
                (now - t0).as_secs_f64()
            );
            last_sent = sent;
            last_t = now;
        }
    });

    // M9 — fully synchronous ingest path.
    //
    // Pre-M9, rayon workers called `rt_handle.block_on(submit_batch)`
    // to cross into tokio and invoke the async `index_batch_turbo_raw`.
    // strace profiling showed 84 % of syscall time was futex, ~40 % of
    // which was this rayon↔tokio crossing (2-4 wake/wait pairs per
    // batch).  Now rayon workers call `idx.index_batch_sync_raw` — a
    // pure synchronous function — with zero tokio involvement.  Back-
    // pressure is expressed inside the engine via `parking_lot::Condvar`
    // (one futex pair per wait) and flush scheduling is owned by a
    // dedicated OS thread (`xerj-flusher-<name>`).
    //
    // The per-batch CLI semaphore that used to bound in-flight work is
    // gone: the engine's Condvar back-pressure already caps memtable
    // size at `3 × flush_threshold`.

    let n_scanners = workers;
    let chunk_size = (data.len() / n_scanners).max(1);

    let mut boundaries: Vec<usize> = Vec::with_capacity(n_scanners + 1);
    boundaries.push(0);
    for i in 1..n_scanners {
        let approx = i * chunk_size;
        if approx >= data.len() {
            break;
        }
        let mut pos = approx;
        while pos < data.len() && data[pos] != b'\n' {
            pos += 1;
        }
        if pos < data.len() {
            pos += 1;
        }
        boundaries.push(pos);
    }
    boundaries.push(data.len());
    boundaries.dedup();

    let _ = &sem; // keep sem in scope for name-shadowing parity; unused on sync path.
    let limit = cmd.limit;
    let idx_for_rayon: std::sync::Arc<xerj_engine::Index> = std::sync::Arc::clone(&index);
    let sent_for_rayon = StdArc::clone(&total_sent);
    let errs_for_rayon = StdArc::clone(&total_errs);

    let scan_result = tokio::task::spawn_blocking(move || {
        use rayon::prelude::*;
        let pairs: Vec<(usize, usize)> = boundaries.windows(2).map(|w| (w[0], w[1])).collect();

        let seen = std::sync::atomic::AtomicU64::new(0);

        pairs
            .par_iter()
            .enumerate()
            .for_each(|(scanner_idx, &(start, end))| {
                if limit > 0 && seen.load(std::sync::atomic::Ordering::Relaxed) >= limit as u64 {
                    return;
                }
                let chunk = &data[start..end];
                let mut cursor = 0usize;
                let mut current: Vec<(String, StdArc<[u8]>)> = Vec::with_capacity(batch_size);

                // Synchronous submit with back-pressure retry.  The engine
                // returns ResourceExhausted only when memtable > 3× flush
                // threshold (after the internal Condvar wait has already
                // let the flusher drain).  Clone the batch up-front because
                // a successful call consumes it; retry uses a sibling clone.
                let submit = |batch: Vec<(String, StdArc<[u8]>)>| {
                    let n = batch.len() as u64;
                    let mut retry: Vec<(String, StdArc<[u8]>)> = batch
                        .iter()
                        .map(|(id, b)| (id.clone(), StdArc::clone(b)))
                        .collect();
                    let mut current_batch = Some(batch);
                    let mut attempts: u32 = 0;
                    loop {
                        let b = current_batch.take().unwrap();
                        match idx_for_rayon.index_batch_sync_raw(b) {
                            Ok(_) => {
                                sent_for_rayon.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
                                return;
                            }
                            Err(xerj_engine::EngineError::Common(
                                xerj_common::XerjError::ResourceExhausted { .. },
                            )) if attempts < 240 => {
                                attempts += 1;
                                // The engine's Condvar already waited up to
                                // 50 ms; add a tiny thread::sleep to avoid
                                // a tight busy-loop on persistent back-
                                // pressure (flusher is clearly saturated).
                                std::thread::sleep(std::time::Duration::from_millis(5));
                                let reclone: Vec<(String, StdArc<[u8]>)> = retry
                                    .iter()
                                    .map(|(id, b)| (id.clone(), StdArc::clone(b)))
                                    .collect();
                                current_batch = Some(std::mem::replace(&mut retry, reclone));
                            }
                            Err(e) => {
                                error!("batch ingest error: {e}");
                                errs_for_rayon.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
                                return;
                            }
                        }
                    }
                };

                while cursor < chunk.len() {
                    let nl_rel =
                        memchr::memchr(b'\n', &chunk[cursor..]).unwrap_or(chunk.len() - cursor);
                    let line_end = cursor + nl_rel;
                    let line = &chunk[cursor..line_end];
                    cursor = line_end + 1;
                    if line.is_empty() {
                        continue;
                    }

                    let seq = seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if limit > 0 && seq >= limit as u64 {
                        break;
                    }

                    let bytes: StdArc<[u8]> = StdArc::from(line);
                    let doc_id = format!("{scanner_idx}_{seq}");
                    current.push((doc_id, bytes));

                    if current.len() >= batch_size {
                        let batch = std::mem::replace(&mut current, Vec::with_capacity(batch_size));
                        submit(batch);
                    }
                }
                if !current.is_empty() {
                    submit(current);
                }
            });
    });

    scan_result.await.context("mmap scan task")?;

    // Capture ingest-only time (WAL + memtable, durable) BEFORE the
    // final drain-to-disk kicks in.
    let ingest_elapsed = start.elapsed();
    let sent_at_ingest = total_sent.load(std::sync::atomic::Ordering::Relaxed);

    reporter.abort();

    // Force a final flush UNCONDITIONALLY so every run finishes
    // segment-durable (not memtable-only).  `flush_all_if_needed`
    // skips the drain when the memtable is below the configured
    // threshold, which made some runs falsely look faster because
    // they left data in memtable instead of on disk.
    let flush_start = std::time::Instant::now();
    if let Err(e) = index.flush().await {
        error!("final flush error: {e}");
    }
    let flush_elapsed = flush_start.elapsed();

    let elapsed = start.elapsed();
    let sent = total_sent.load(std::sync::atomic::Ordering::Relaxed);
    let errs = total_errs.load(std::sync::atomic::Ordering::Relaxed);
    let total_rate = sent as f64 / elapsed.as_secs_f64();
    let ingest_rate = sent_at_ingest as f64 / ingest_elapsed.as_secs_f64();

    println!();
    println!("═══════════════════════════════════════════════════════════");
    println!(" xerj index: complete");
    println!("═══════════════════════════════════════════════════════════");
    println!(" index          : {}", cmd.index);
    println!(" file           : {}", cmd.file.display());
    println!(" file size      : {} MB", file_size / (1024 * 1024));
    println!(" docs sent      : {}", sent);
    println!(" errors         : {}", errs);
    println!(" ingest time    : {:.2} s", ingest_elapsed.as_secs_f64());
    println!(
        " ingest rate    : {:.0} docs/s  (WAL-durable, in-memtable)",
        ingest_rate
    );
    println!(" final flush    : {:.2} s", flush_elapsed.as_secs_f64());
    println!(" total elapsed  : {:.2} s", elapsed.as_secs_f64());
    println!(
        " total rate     : {:.0} docs/s  (fully segment-durable)",
        total_rate
    );
    println!(" workers        : {}", workers);
    println!(" batch size     : {}", batch_size);
    println!("═══════════════════════════════════════════════════════════");

    Ok(())
}

/// Process entry point.
///
/// Builds the Tokio runtime EXPLICITLY (rather than via `#[tokio::main]`) so we
/// can provision the worker-thread count deliberately.
///
/// ## Why an over-provisioned worker pool (read-under-write p95 tail)
///
/// The CPU-heavy search body runs via `tokio::task::block_in_place` (see
/// `Index::search`, M5.21): it converts the *current* runtime worker into a
/// blocking thread for the duration of the query and asks the scheduler to
/// keep the async side alive on the remaining workers. That is fine at low
/// concurrency, but the mixed read-under-write benchmark fires 300 reads/s
/// OPEN-LOOP while a full-speed bulk writer (~128 k docs/s) drives periodic
/// flush + merge *finalize* windows. During those windows individual reads
/// slow enough that several are `block_in_place`d at once; with only
/// `ncpus` core threads (the `#[tokio::main]` default) enough of them convert
/// simultaneously that the runtime can no longer promptly poll the IO reactor
/// to ACCEPT and dispatch NEW connections/requests.
///
/// This was isolated by profiling: for the slowest client requests the server-
/// side `took` is 0 ms (the search itself did no work), and even a trivial
/// `GET /` (no engine work at all) shows the same 45–100 ms p95/p99 tail under
/// the writer — i.e. the latency is in accept/dispatch, not compute. It is
/// invariant to renicing / CPU-pinning the ingest+flush+merge pools and to
/// reserving idle cores (measured: no effect), because the stall is the async
/// runtime running out of workers to drive the reactor, not a CPU shortage.
///
/// Provisioning ~8× cores worker threads keeps ample spare workers to drive the
/// reactor through those windows. Measured on the 32-core bench (mixed repro,
/// keep-alive `http.Agent` transport identical to `bench-matrix.mjs`): the
/// mixed read **p95** roughly halves (e.g. match_all 45→30 ms, bool 43→8 ms,
/// terms 44→34 ms) across repeated runs, with **ingest throughput unchanged**
/// (1 M×c8: 431–438 k → 447–457 k docs/s) and reads byte-identical. Idle
/// workers park in `epoll_wait` (≈0 CPU; only ~2 MB *virtual* stack each), so
/// there is no cost when concurrency is low.
///
/// HONEST LIMIT: this halves the p95 but does NOT close the p99 gap to ES's
/// single-digit ms. The deep p99 stalls are *process-global* (all threads
/// briefly block in the kernel during the flush/merge drain), so no amount of
/// user-space worker provisioning dispatches around them — that residual is a
/// structural consequence of XERJ ingesting ~5× faster than ES and is not
/// addressable without throttling ingest.
///
/// Override with `TOKIO_WORKER_THREADS` (operators on very small or very large
/// hosts, or wanting the stock `ncpus` behaviour, can set it explicitly).
/// Raise the process open-file limit toward the OS hard limit before anything
/// else runs. Every index holds segment mmaps, a WAL and a merge task, so
/// discovering a large source tree (Django infers ~73 datasets → ~73 indices)
/// opens thousands of descriptors. On a default macOS soft limit (often 256)
/// the server exhausts them mid-run and index creation fails with
/// `Too many open files` (EMFILE, os error 24) — reproduced against a real
/// `django` clone. Bump the soft limit to the hard limit, stepping down if the
/// kernel (`kern.maxfilesperproc` on macOS) rejects the target. Best-effort:
/// any failure here is non-fatal (the server just runs with the inherited cap).
#[cfg(unix)]
fn raise_nofile_limit() {
    unsafe {
        let mut lim = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0 {
            return;
        }
        for (cur, max) in nofile_plan(lim.rlim_cur, lim.rlim_max) {
            let next = libc::rlimit {
                rlim_cur: cur,
                rlim_max: max,
            };
            if libc::setrlimit(libc::RLIMIT_NOFILE, &next) == 0 {
                return; // raised
            }
        }
    }
}

/// Restore the default SIGPIPE disposition, which the Rust runtime replaces
/// with `SIG_IGN` before `main` runs. Under `SIG_IGN` a write to a pipe whose
/// reader has gone returns `EPIPE`, `println!` turns that into a panic
/// ("failed printing to stdout: Broken pipe"), and the release profile's
/// `panic = "abort"` turns THAT into a core dump — so `xerj autoindex map |
/// head -80`, and every other `xerj … | head`, dumped core and lost the
/// command's real exit status. With `SIG_DFL` the kernel terminates the
/// process on the signal instead: no panic, no core, and the conventional
/// 128+13 status that shells report as 141.
///
/// Process-global, so it reaches the server too, where the guarantee is
/// narrower than the syscall alone would suggest. A plain socket write goes out
/// as `send()` with `MSG_NOSIGNAL` (`SO_NOSIGPIPE` on macOS) and surfaces
/// `EPIPE`, but `write_vectored` lowers to `writev()`, which takes no flags
/// argument and so cannot carry it: a vectored write to an RST-broken socket
/// under `SIG_DFL` does die on the signal. What protects the server is the
/// runtime, not the write path — tokio/hyper is readiness-driven, observes the
/// peer's reset on the read side, and tears the connection down before reaching
/// a body `writev`. Confirmed empirically under sustained mid-write aborts
/// (640+ RSTs, none of them signalled us), so it holds for this architecture
/// rather than for socket writes in general. Its own stdout does change — the
/// startup banner no longer aborts when the pipe has no reader, but tracing
/// output, which discards write errors, no longer keeps a server alive past a
/// vanished stdout either: the next log line terminates it. Supervised
/// deployments point stdout at a file or `/dev/null` (`brain`'s boot path
/// included), so in practice this only reaches `xerj | reader-that-leaves`.
#[cfg(unix)]
fn restore_default_sigpipe() {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

/// The ordered `(rlim_cur, rlim_max)` pairs to try for RLIMIT_NOFILE, highest
/// first. Pure, so the macOS-only `RLIM_INFINITY` path is unit-testable without
/// a Mac. Every pair uses `rlim_max == rlim_cur` — a CONCRETE value: macOS
/// REJECTS `setrlimit(RLIMIT_NOFILE)` when `rlim_max` is `RLIM_INFINITY` (its
/// launchd default), so the earlier "pass the inherited hard limit back"
/// version was a silent no-op there and the limit stayed at 256. Steps down by
/// halving for the macOS `kern.maxfilesperproc` ceiling until one is accepted.
#[cfg(unix)]
fn nofile_plan(cur: libc::rlim_t, max: libc::rlim_t) -> Vec<(libc::rlim_t, libc::rlim_t)> {
    // Cap an "infinite" hard limit to concrete headroom (1,048,576).
    let hard = if max == libc::RLIM_INFINITY {
        1_048_576
    } else {
        max
    };
    if hard <= cur {
        return Vec::new(); // already at the ceiling
    }
    let mut plan = Vec::new();
    let mut want = hard;
    loop {
        plan.push((want, want));
        if want <= cur.saturating_add(1) {
            break;
        }
        want = (want / 2).max(cur + 1);
    }
    plan
}

#[cfg(all(test, unix))]
mod nofile_tests {
    use super::nofile_plan;

    #[test]
    fn infinity_hard_yields_only_concrete_maxes() {
        // The macOS case the earlier version silently failed on.
        let plan = nofile_plan(256, libc::RLIM_INFINITY);
        assert!(!plan.is_empty());
        assert_eq!(plan[0], (1_048_576, 1_048_576)); // highest first, concrete
        assert!(plan
            .iter()
            .all(|&(c, m)| c == m && m != libc::RLIM_INFINITY));
        for w in plan.windows(2) {
            assert!(w[0].0 > w[1].0, "must step strictly down");
        }
        assert!(
            plan.last().unwrap().0 <= 257,
            "ends just above the soft limit"
        );
    }

    #[test]
    fn concrete_hard_is_targeted_directly() {
        assert_eq!(nofile_plan(256, 524_288)[0], (524_288, 524_288));
    }

    #[test]
    fn already_at_ceiling_is_noop() {
        assert!(nofile_plan(524_288, 524_288).is_empty());
        assert!(nofile_plan(1024, 512).is_empty());
    }
}

fn main() -> Result<()> {
    // Before anything can write to stdout, including the `--help`/`--version`
    // fast paths inside `parse_args`.
    #[cfg(unix)]
    restore_default_sigpipe();

    #[cfg(unix)]
    raise_nofile_limit();

    // Dispatch before creating Tokio's pool so each PDF worker stays small.
    if matches!(std::env::args().nth(1).as_deref(), Some("__extract-pdf")) {
        std::process::exit(xerj_autoindex::extract::pdf::run_worker_cli());
    }
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8);
    // ~8× cores, clamped to a sane band; env override wins.
    let worker_threads = std::env::var("TOKIO_WORKER_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(|| (cores * 8).clamp(64, 512));
    let rt = build_runtime(worker_threads)?;
    rt.block_on(async_main())
}

/// Explicit worker-thread stack size for the production Tokio runtime.
///
/// Tokio's default is 2 MiB. We pin 4 MiB deliberately so the deepest
/// legitimate-but-adversarial request rides on documented headroom rather than
/// on the default staying ahead of demand. The budget on ONE worker stack,
/// deepest frame first:
///
/// * The Painless script evaluator's own worst case — a self-application
///   closure recursed to `MAX_CALL_DEPTH` (32) with every body at the parser's
///   maximum block nesting — consumes a *measured* ~1.20 MiB (1,262,320 bytes);
///   see `xerj_engine`'s `painless::MAX_CALL_DEPTH` stackprobe table. That
///   figure is intrinsic to the depth ceiling and does NOT shrink with a
///   bigger stack.
/// * Underneath the evaluator sits everything that carried the request there:
///   the axum accept/route frames, the two-layer authorization middleware
///   (#79), the `_*_by_query` handler, the search entry, `block_in_place`, and
///   the segment scan.
///
/// At the old 2 MiB default the worst case left only ~0.8 MiB
/// (2,097,152 − 1,262,320 = 834,832 bytes) for that whole "underneath" stack,
/// and the authz layer now competes for it. At 4 MiB the headroom is ~2.80 MiB
/// (4,194,304 − 1,262,320 = 2,931,984 bytes), which absorbs the middleware and
/// handler frames with a comfortable margin. Idle workers park in `epoll_wait`
/// and only reserve *virtual* address space for the stack, so the larger size
/// costs nothing at low concurrency.
const RT_THREAD_STACK_SIZE: usize = 4 * 1024 * 1024;

/// Compile-time guardrails on the stack budget above. Enforced in the real
/// build (not only under `cfg(test)`), so a future edit that drops the stack
/// back to the tokio default, or that lets the "underneath" headroom fall
/// below the axum + authz + by-query + search frames, fails to compile.
const _: () = {
    // Measured Painless closure-recursion worst case at `MAX_CALL_DEPTH`, from
    // painless.rs's stackprobe table (40,720 B/level × 31 increments). Kept in
    // sync with `xerj_engine::painless::MAX_CALL_DEPTH`'s documented figure.
    const PAINLESS_WORST_CASE: usize = 1_262_320;
    // Must be explicitly raised off tokio's 2 MiB default.
    assert!(RT_THREAD_STACK_SIZE >= 4 * 1024 * 1024);
    // Must leave > 1 MiB under the evaluator worst case for the frames beneath
    // it (axum + authz middleware + by-query handler + search + segment scan).
    assert!(RT_THREAD_STACK_SIZE - PAINLESS_WORST_CASE > 1024 * 1024);
};

/// Build the production multi-threaded Tokio runtime with the deliberate worker
/// count and [`RT_THREAD_STACK_SIZE`]. Factored out of `main` so the exact
/// builder configuration the server boots with is exercised by a test.
fn build_runtime(worker_threads: usize) -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(worker_threads)
        .thread_stack_size(RT_THREAD_STACK_SIZE)
        .thread_name("xerj-rt")
        .build()
        .context("build tokio runtime")
}

#[cfg(test)]
mod runtime_tests {
    use super::build_runtime;

    #[test]
    fn runtime_builds_and_boots() {
        // The exact builder config `main` uses must construct and run a task on
        // a 4 MiB-stack worker — this is the boot smoke test for the setting.
        let rt = build_runtime(2).expect("prod runtime builds");
        assert_eq!(rt.block_on(async { 40 + 2 }), 42);
    }
}

async fn async_main() -> Result<()> {
    // Subcommand dispatch — check argv[1] before any other parsing.
    let argv1 = std::env::args().nth(1);
    if matches!(argv1.as_deref(), Some("index")) {
        let cmd = parse_index_args();
        return run_cli_index(cmd).await;
    }
    if matches!(argv1.as_deref(), Some("autoindex")) {
        // Zero-config folder discovery + indexing over the ES-compat API.
        // Fully synchronous internally — run it off the async runtime.
        let code = tokio::task::spawn_blocking(xerj_autoindex::run_cli)
            .await
            .unwrap_or(1);
        std::process::exit(code);
    }
    if matches!(argv1.as_deref(), Some("brain")) {
        // One command → running, browsable second brain: boots/attaches the
        // server, reuses the autoindex pipeline with graph detection on,
        // then opens the console. Fully synchronous internally — run it off
        // the async runtime like `autoindex`.
        let code = tokio::task::spawn_blocking(brain::run_cli)
            .await
            .unwrap_or(1);
        std::process::exit(code);
    }

    // Start profiling before config loading, TLS, engine replay, and router
    // construction so delay=0 is a genuine initialization profile.
    #[cfg(all(target_os = "linux", feature = "debug-profiling"))]
    let _debug_profiles = debug_profiling::spawn()?;

    // 0. Record startup time as early as possible.
    let startup_start = std::time::Instant::now();

    // 1. CLI args
    let args = parse_args();

    // 2. Config — loaded before tracing so the [logging] format (text/json)
    //    governs the very first startup line (RC4-W4 item 6). A config-load
    //    failure still surfaces via the returned Result → stderr.
    let mut cfg = load_config(&args)?;

    // 3. Tracing
    init_tracing(&cfg.logging);

    info!("xerj v{} starting", env!("CARGO_PKG_VERSION"));

    // 4. Data directory
    std::fs::create_dir_all(&cfg.server.data_dir)
        .with_context(|| format!("create data dir {}", cfg.server.data_dir))?;

    // 5. Admin key (first-run)
    ensure_admin_key(&mut cfg)?;

    // 6. TLS certificate
    if let Err(e) = ensure_tls_cert(&mut cfg) {
        error!("TLS setup failed ({e:#}) — falling back to plain HTTP");
        cfg.tls.enabled = false;
    }

    // 6b. In-process TLS config (rustls).  Loaded once here and shared
    //     (cheap Arc clone) by both listeners.  When TLS is enabled but the
    //     cert fails to load this returns an error and startup aborts —
    //     we do NOT silently downgrade an operator-requested HTTPS port to
    //     cleartext.
    let tls_config = build_tls_config(&cfg)
        .await
        .context("build TLS server config")?;

    // 7. Metrics
    let metrics = Metrics::new().context("initialise metrics")?;

    // 8. Engine (opens existing indices from disk)
    let engine = Engine::new(cfg.clone()).context("initialise engine")?;

    // 8b. Cluster runner (if cluster mode is enabled)
    let _cluster_shutdown = if cfg.cluster.enabled {
        // Fail closed (issue #75). The cluster port carries Raft control
        // messages; without a shared secret every frame on it is
        // unauthenticated, so refuse to start rather than open it. This must
        // abort startup — it is deliberately NOT folded into the degraded
        // single-node fallback below, which only covers bind failures.
        let secret_material = cfg.cluster.effective_auth_secret().ok_or_else(|| {
            anyhow::anyhow!(
                "cluster.enabled = true but no cluster auth secret is configured: set \
                 cluster.auth_secret or {}. Refusing to start an unauthenticated cluster \
                 transport.",
                xerj_common::config::ClusterConfig::AUTH_SECRET_ENV
            )
        })?;
        let cluster_secret = xerj_cluster::ClusterSecret::new(&secret_material)
            .context("cluster auth secret rejected")?;

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        // Parse peer list: "node_id=host:port" entries.
        let mut peers = std::collections::HashMap::new();
        for peer_str in &cfg.cluster.peers {
            if let Some((id, addr_str)) = peer_str.split_once('=') {
                match addr_str.parse::<std::net::SocketAddr>() {
                    Ok(addr) => {
                        peers.insert(id.to_string(), addr);
                    }
                    Err(e) => warn!("cluster: ignoring invalid peer {peer_str}: {e}"),
                }
            } else {
                warn!("cluster: ignoring malformed peer entry {peer_str} (expected id=host:port)");
            }
        }

        // Derive this node's listen address from the server bind address + cluster port.
        let node_id = format!("{}:{}", cfg.server.bind_address, cfg.cluster.port);
        let listen_addr: std::net::SocketAddr =
            format!("{}:{}", cfg.server.bind_address, cfg.cluster.port)
                .parse()
                .context("parse cluster listen address")?;

        let tick = std::time::Duration::from_millis(cfg.cluster.tick_ms);

        match TcpTransport::new(node_id.clone(), listen_addr, peers, cluster_secret).await {
            Ok(transport) => {
                let peer_ids: Vec<String> = cfg
                    .cluster
                    .peers
                    .iter()
                    .filter_map(|p| p.split_once('=').map(|(id, _)| id.to_string()))
                    .collect();

                let node = ClusterNode::new(node_id.clone(), peer_ids, Box::new(transport));
                let mut runner = ClusterRunner::new(node, tick, shutdown_rx);

                info!(node = %node_id, "Starting cluster runner (Raft mode)");
                tokio::spawn(async move { runner.run().await });
            }
            Err(e) => {
                error!("Failed to start cluster TCP transport: {e:#}");
                warn!("Continuing in degraded single-node mode");
            }
        }

        Some(shutdown_tx)
    } else {
        info!("Cluster mode disabled — running in single-node mode");
        None
    };

    // 9. Xerj Console bootstrap.  Creates `.xerj_*` system indices on first
    //    boot, persists a 32-byte master key under data_dir/.xerj_master_key
    //    (mode 0600), and prints the first-launch magic-link banner to
    //    stderr if no active user exists yet.  Idempotent on reboot.
    let xerj_console_bind_url = format!(
        "http://{}:{}",
        if cfg.server.bind_address == "0.0.0.0" || cfg.server.bind_address == "::" {
            "localhost"
        } else {
            cfg.server.bind_address.as_str()
        },
        cfg.server.es_compat_port,
    );
    let xerj_console_outcome = xerj_console_api::bootstrap::run(
        &engine,
        Path::new(&cfg.server.data_dir),
        &xerj_console_bind_url,
    )
    .await
    .context("xerj-console bootstrap")?;

    let xerj_console_node_id: String = if cfg.cluster.enabled {
        format!("{}:{}", cfg.server.bind_address, cfg.cluster.port)
    } else {
        "local".to_string()
    };
    let xerj_console_cluster_mode = if cfg.cluster.enabled {
        ClusterMode::Raft
    } else {
        ClusterMode::Standalone
    };
    // #76 S5-4: which reverse proxies (if any) may tell us who the client is.
    // Empty by default — an unconfigured node ignores `x-forwarded-for`
    // entirely and keys the auth rate limiter on the TCP peer. `validate()`
    // already rejected malformed entries, so this parse cannot fail; treat a
    // surprise as fatal rather than silently widening or narrowing trust.
    let trusted_proxies = xerj_common::net::TrustedProxies::parse(&cfg.server.trusted_proxies)
        .map_err(|why| anyhow::anyhow!("server.trusted_proxies: {why}"))?;
    if !trusted_proxies.is_empty() {
        info!(
            "trusting x-forwarded-for from {} configured proxy entr{}",
            cfg.server.trusted_proxies.len(),
            if cfg.server.trusted_proxies.len() == 1 {
                "y"
            } else {
                "ies"
            }
        );
    }
    let xerj_console_state = ConsoleState::new(
        engine.clone(),
        xerj_console_node_id,
        xerj_console_outcome.master_key,
        xerj_console_cluster_mode,
    )
    .with_trusted_proxies(trusted_proxies);

    // 9b. Application state
    let state = AppState::new(cfg.clone(), engine, metrics);

    // 9b-i. Observability wiring (RC4 W4 item 2).
    //   (a) Install the process-wide metrics handle the engine records
    //       flush/merge/WAL/index-latency observations into — the flush, merge
    //       and WAL write sites live deep in xerj-engine, which is built before
    //       `Metrics` exists and holds no handle to it.
    //   (b) Spawn the 10 s gauge refresher that keeps doc_count / segment_count
    //       / wal_size_bytes / memory_usage live (otherwise a flat zero even at
    //       millions of docs, hiding the RSS-runaway and WAL-growth signals).
    xerj_engine::set_engine_metrics(state.metrics.clone());
    tokio::spawn(xerj_api::es_compat::run_metrics_gauge_loop(state.clone()));

    // 9c. Routers — engine and Xerj Console are *peer* surfaces.  Each crate
    //     builds a complete Router (routes + its own auth + its own
    //     middleware) and `xerj-server` merges them onto the same TCP
    //     listeners.  Engine layers (admin Bearer auth) apply only to
    //     engine routes; Xerj Console's session-cookie auth applies only to
    //     /_xerj-console/api/v1/* routes.  Yanking xerj-console-api out of this
    //     merge would leave xerj-api compiling and serving on its own
    //     — a property worth preserving.
    let xerj_console_router = xerj_console_api::xerj_console_router(xerj_console_state);
    let native_router = build_native_router(state.clone()).merge(xerj_console_router.clone());
    let es_router = build_es_compat_router(state.clone()).merge(xerj_console_router);

    // 10. Banner (includes total startup time)
    let startup_ms = startup_start.elapsed().as_millis();
    info!("startup complete in {}ms", startup_ms);
    print_banner(&cfg, startup_ms);

    // 11. Bind addresses
    let bind = &cfg.server.bind_address;
    let rest_addr: SocketAddr = format!("{}:{}", bind, cfg.server.rest_port)
        .parse()
        .context("parse REST bind address")?;
    let es_addr: SocketAddr = format!("{}:{}", bind, cfg.server.es_compat_port)
        .parse()
        .context("parse ES-compat bind address")?;
    let grpc_addr: SocketAddr = format!("{}:{}", bind, cfg.server.grpc_port)
        .parse()
        .context("parse gRPC bind address")?;

    // 12. Background flush timer
    let flusher = spawn_periodic_flusher(
        state.engine.clone(),
        std::time::Duration::from_secs(cfg.storage.flush_interval_secs),
    );

    // 12b. Resource governor sampler (RC4 W3 items 1 & 3): refreshes the
    //      process-wide memtable / RSS / disk-usage atomics that drive the
    //      parent circuit breaker and the disk flood-stage write block. This
    //      is the structural guard against the 112 GiB OOM class — writes get
    //      a 429 circuit_breaking_exception before the kernel OOM-kills us.
    state.engine.spawn_resource_sampler();

    let _ingest_memory_trace = ingest_memory_trace::spawn(&state.engine);
    // 13. Start servers concurrently
    let rest_tls = tls_config.clone();
    let rest = tokio::spawn(async move {
        if let Err(e) = serve(native_router, rest_addr, "native REST", rest_tls).await {
            error!("native REST: {e:#}");
        }
    });

    let es_tls = tls_config.clone();
    let es = tokio::spawn(async move {
        if let Err(e) = serve(es_router, es_addr, "ES-compat", es_tls).await {
            error!("ES-compat: {e:#}");
        }
    });

    // Real tonic XerjSearch service. Exits on the same SIGTERM/SIGINT as the
    // REST listeners so `tokio::join!` below returns and the shutdown flush
    // hook runs. A bind/transport failure is logged, not fatal.
    let grpc_state = state.clone();
    let grpc = tokio::spawn(async move {
        if let Err(e) = grpc::serve_grpc(grpc_addr, grpc_state, shutdown_signal()).await {
            error!("gRPC server: {e:#}");
        }
    });

    // 14. Wait for all servers (they exit together on shutdown)
    let _ = tokio::join!(rest, es, grpc);

    // Servers are down — stop the periodic flusher BEFORE the final
    // synchronous flush below so its next tick can't race
    // `flush_all_force` during shutdown.
    //
    // RC4 W2 #20: this abort used to sit ABOVE the `join!`, killing the
    // flusher milliseconds after it was spawned.  `flush_interval_secs`
    // was inert for the entire life of the server: an over-threshold
    // memtable that the write-path flush scheduler missed (WAL replay at
    // boot, flush-permit exhaustion on the last write) sat in memory —
    // pinning its WAL generations on disk — until shutdown or the next
    // explicit `_flush`.
    flusher.abort();

    // 15. Final synchronous flush across every index.
    //
    // The graceful-shutdown future fires on SIGTERM/SIGINT and stops axum
    // from accepting new connections, but it does NOT drain the engine's
    // in-memory memtables to durable segments.  Without this final pass,
    // any docs that were bulk-ingested after the last auto-flush threshold
    // crossing live only in the WAL until the next startup — and an index
    // whose memtable never reached that threshold (small batches, brief
    // sessions) loses 100 % of its data on restart because startup index
    // discovery looks for segment-bearing directories first.  See POV
    // report 2026-04-24T23-58-00 (B-2) for the full failure mode.
    info!("flushing in-memory state before exit…");
    let flush_started = std::time::Instant::now();
    state.engine.flush_all_force().await;
    info!("final flush complete in {:.0?}", flush_started.elapsed());

    info!("xerj v{} stopped. Goodbye.", env!("CARGO_PKG_VERSION"));
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────
//
// These live as an in-crate unit test module (not `tests/tls.rs`) because
// `serve` / `build_tls_config` / `ensure_tls_cert` are private items of a
// *binary* crate — a `tests/` integration test compiles as a separate crate
// and cannot reach them.  A unit test can, so it exercises the real listener
// wiring end to end (self-signed cert → HTTPS round-trip).
#[cfg(test)]
mod tls_tests {
    use super::*;
    use axum::routing::get;

    /// Grab an ephemeral port, then release it so `serve` can rebind.
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn test_router() -> Router {
        Router::new()
            .route("/", get(|| async { "ok" }))
            // Echoes the transport peer so a test can prove `serve` installed
            // `ConnectInfo` — the identity the auth rate limiter keys on
            // (#76 S5-4). Without connect-info wiring this handler would 500
            // (missing extension), which is exactly the regression to catch.
            .route(
                "/peer",
                get(
                    |axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<SocketAddr>| async move {
                        peer.ip().to_string()
                    },
                ),
            )
    }

    /// Poll until the just-spawned listener answers (spawn races the client).
    async fn poll_status(client: &reqwest::Client, url: &str) -> u16 {
        for _ in 0..100 {
            if let Ok(resp) = client.get(url).send().await {
                return resp.status().as_u16();
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("server never became reachable at {url}");
    }

    /// TLS enabled: the self-signed cert `ensure_tls_cert` writes is loaded
    /// by `build_tls_config` and terminated in-process by `serve`; an HTTPS
    /// GET returns 200.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn https_serves_when_tls_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.server.data_dir = dir.path().to_string_lossy().into_owned();
        cfg.tls.enabled = true;

        ensure_tls_cert(&mut cfg).expect("generate self-signed cert");
        let tls = build_tls_config(&cfg)
            .await
            .expect("build tls config")
            .expect("tls enabled must yield Some");

        let port = free_port();
        let addr: SocketAddr = ([127, 0, 0, 1], port).into();
        let server = tokio::spawn(serve(test_router(), addr, "test-tls", Some(tls)));

        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true) // self-signed
            .build()
            .unwrap();
        let status = poll_status(&client, &format!("https://127.0.0.1:{port}/")).await;
        assert_eq!(status, 200, "HTTPS GET / should return 200");

        // Plain HTTP against the TLS port must NOT succeed as cleartext.
        let plain = reqwest::Client::new();
        let plain_res = plain
            .get(format!("http://127.0.0.1:{port}/"))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await;
        assert!(
            plain_res.is_err(),
            "cleartext GET against the TLS port must fail, got {plain_res:?}"
        );

        server.abort();
    }

    /// TLS disabled: the unchanged plain-HTTP path still serves.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn http_serves_when_tls_disabled() {
        let port = free_port();
        let addr: SocketAddr = ([127, 0, 0, 1], port).into();
        let server = tokio::spawn(serve(test_router(), addr, "test-plain", None));

        let client = reqwest::Client::new();
        let status = poll_status(&client, &format!("http://127.0.0.1:{port}/")).await;
        assert_eq!(status, 200, "HTTP GET / should return 200");

        server.abort();
    }

    /// #76 S5-4: the plain listener must hand handlers the real TCP peer.
    /// The console rate limiter keys on it; if `ConnectInfo` is missing the
    /// limiter degrades to one shared bucket and, worse, the only remaining
    /// signal would be the caller-supplied header we are refusing to trust.
    /// A spoofed `x-forwarded-for` must not perturb what the transport says.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn plain_listener_exposes_the_real_peer_not_the_header() {
        let port = free_port();
        let addr: SocketAddr = ([127, 0, 0, 1], port).into();
        let server = tokio::spawn(serve(test_router(), addr, "test-peer-plain", None));

        let client = reqwest::Client::new();
        assert_eq!(
            poll_status(&client, &format!("http://127.0.0.1:{port}/")).await,
            200
        );

        let body = client
            .get(format!("http://127.0.0.1:{port}/peer"))
            .header("x-forwarded-for", "1.2.3.4")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(
            body, "127.0.0.1",
            "handler must see the socket peer, not the spoofed header"
        );

        server.abort();
    }

    /// Same guarantee on the TLS path — it uses a different accept loop
    /// (`axum_server`), so connect-info has to be wired there independently.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn tls_listener_exposes_the_real_peer_not_the_header() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.server.data_dir = dir.path().to_string_lossy().into_owned();
        cfg.tls.enabled = true;
        ensure_tls_cert(&mut cfg).expect("generate self-signed cert");
        let tls = build_tls_config(&cfg)
            .await
            .expect("build tls config")
            .expect("tls enabled must yield Some");

        let port = free_port();
        let addr: SocketAddr = ([127, 0, 0, 1], port).into();
        let server = tokio::spawn(serve(test_router(), addr, "test-peer-tls", Some(tls)));

        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap();
        assert_eq!(
            poll_status(&client, &format!("https://127.0.0.1:{port}/")).await,
            200
        );

        let body = client
            .get(format!("https://127.0.0.1:{port}/peer"))
            .header("x-forwarded-for", "1.2.3.4")
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(
            body, "127.0.0.1",
            "TLS handler must see the socket peer, not the spoofed header"
        );

        server.abort();
    }

    /// TLS enabled but the cert path is missing → `build_tls_config` fails
    /// loud rather than silently downgrading to cleartext.
    #[tokio::test]
    async fn build_tls_config_fails_loud_on_missing_cert() {
        let mut cfg = Config::default();
        cfg.tls.enabled = true;
        cfg.tls.cert_path = "/nonexistent/xerj.crt".to_string();
        cfg.tls.key_path = "/nonexistent/xerj.key".to_string();

        let res = build_tls_config(&cfg).await;
        assert!(
            res.is_err(),
            "missing cert with TLS enabled must error, not downgrade"
        );
    }
}

/// RC4 W2 #20/#21 regression tests. Like `tls_tests` above, these live in
/// the binary crate because `spawn_periodic_flusher` / `write_secret_file`
/// are private items of `main.rs`.
#[cfg(test)]
mod server_correctness_tests {
    use super::*;
    use tempfile::TempDir;
    use xerj_common::types::Schema;

    /// RC4 W2 #20: the periodic flusher must actually flush an
    /// over-threshold memtable on its interval.
    ///
    /// Phase 1 lands ~2 MiB of docs under huge thresholds (memtable + WAL
    /// only, no segments) and drops the engine WITHOUT the shutdown flush —
    /// exactly what a crash leaves behind. Phase 2 reopens the same data
    /// dir with a 1 MiB flush threshold: WAL replay leaves the memtable
    /// over threshold with no writes arriving, so the ONLY mechanism that
    /// can flush it is the periodic flusher. Before the fix,
    /// `flusher.abort()` ran before `tokio::join!`, so this state persisted
    /// until shutdown and the flush below never happened.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn periodic_flusher_flushes_replayed_over_threshold_memtable() {
        let dir = TempDir::new().unwrap();
        let data_dir = dir.path().to_str().unwrap().to_string();

        // Phase 1: memtable + WAL only, then drop without flushing.
        {
            let mut cfg = Config::default();
            cfg.server.data_dir = data_dir.clone();
            cfg.storage.flush_size_mb = 4096; // never cross in this phase
            cfg.storage.flush_interval_secs = 3600;
            let engine = Engine::new(cfg).unwrap();
            engine.create_index("t20", Schema::empty()).unwrap();
            let idx = engine.get_index("t20").unwrap();
            let body = "x".repeat(1024);
            for i in 0..2048u32 {
                idx.index_document(
                    Some(i.to_string()),
                    serde_json::json!({ "body": body, "n": i }),
                )
                .await
                .unwrap();
            }
            let stats = engine.index_stats("t20").await.unwrap();
            assert_eq!(
                stats.segment_count, 0,
                "phase 1 must stay memtable-only (raise thresholds if this fires)"
            );
        }

        // Phase 2: reopen with a 1 MiB threshold — replay puts the
        // memtable over threshold; no writes arrive.
        let mut cfg = Config::default();
        cfg.server.data_dir = data_dir;
        cfg.storage.flush_size_mb = 1;
        cfg.storage.flush_interval_secs = 3600; // irrelevant; we drive our own timer
        let engine = std::sync::Arc::new(Engine::new(cfg).unwrap());

        // WAL replay alone must not flush — otherwise this test would pass
        // without the flusher and prove nothing.
        let stats = engine.index_stats("t20").await.unwrap();
        assert_eq!(
            stats.segment_count, 0,
            "WAL replay must not flush by itself"
        );

        let flusher = spawn_periodic_flusher(engine.clone(), std::time::Duration::from_millis(100));

        let mut flushed = false;
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if engine.index_stats("t20").await.unwrap().segment_count > 0 {
                flushed = true;
                break;
            }
        }
        flusher.abort();
        assert!(
            flushed,
            "periodic flusher never flushed the over-threshold replayed memtable"
        );
    }

    /// RC4 W2 #21: secrets written by the server (admin API key, TLS
    /// private key) must be owner-readable only, and a pre-existing
    /// group/world-readable secret from an earlier version must be
    /// tightened when rewritten.
    #[cfg(unix)]
    #[test]
    fn write_secret_file_creates_owner_only_and_tightens_existing() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();

        // Fresh file: created 0600 regardless of umask.
        let fresh = dir.path().join("admin.key");
        write_secret_file(&fresh, b"sekrit").unwrap();
        let mode = std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "fresh secret must be 0600, got {mode:o}");
        assert_eq!(std::fs::read(&fresh).unwrap(), b"sekrit");

        // Pre-existing 0664 file (as written by versions before this fix):
        // rewriting it must tighten the mode, not inherit it.
        let stale = dir.path().join("xerj.key");
        std::fs::write(&stale, b"old").unwrap();
        let mut perm = std::fs::metadata(&stale).unwrap().permissions();
        perm.set_mode(0o664);
        std::fs::set_permissions(&stale, perm).unwrap();

        write_secret_file(&stale, b"new").unwrap();
        let mode = std::fs::metadata(&stale).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "rewritten secret must be tightened to 0600");
        assert_eq!(std::fs::read(&stale).unwrap(), b"new");
    }
}
