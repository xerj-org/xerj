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
static ALLOC: ReportingJemalloc = ReportingJemalloc;

/// Jemalloc plus a FATAL line when an allocation returns null.
///
/// `handle_alloc_error` still runs afterwards and aborts, which raises
/// SIGABRT. The line is written first, with `write(2)`, because the heap
/// is the thing that just failed. SIGKILL cannot be reported.
#[cfg(not(target_env = "msvc"))]
struct ReportingJemalloc;

#[cfg(not(target_env = "msvc"))]
unsafe impl std::alloc::GlobalAlloc for ReportingJemalloc {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        let ptr = tikv_jemallocator::Jemalloc.alloc(layout);
        if ptr.is_null() {
            note_alloc_failure(layout.size(), layout.align());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: std::alloc::Layout) -> *mut u8 {
        let ptr = tikv_jemallocator::Jemalloc.alloc_zeroed(layout);
        if ptr.is_null() {
            note_alloc_failure(layout.size(), layout.align());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        tikv_jemallocator::Jemalloc.dealloc(ptr, layout);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, new_size: usize) -> *mut u8 {
        let ptr = tikv_jemallocator::Jemalloc.realloc(ptr, layout, new_size);
        if ptr.is_null() {
            note_alloc_failure(new_size, layout.align());
        }
        ptr
    }
}

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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Router;
use axum_server::tls_rustls::RustlsConfig;
use tokio::net::TcpListener;
use tokio::signal;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use xerj_api::{build_es_compat_router_with_hub, build_native_router, AppState};
use xerj_cluster::{transport::TcpTransport, ClusterNode, ClusterRunner};
use xerj_common::{config::Config, metrics::Metrics};
use xerj_console_api::{state::ClusterMode, ConsoleState, RpConfig};
use xerj_engine::Engine;

mod brain;
#[cfg(all(target_os = "linux", feature = "debug-profiling"))]
mod debug_profiling;
mod grpc;
mod grpc_hub;
mod ingest_memory_trace;
mod share;

// ─────────────────────────────────────────────────────────────────────────────
// CLI
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct CliArgs {
    config: Option<PathBuf>,
    data_dir: Option<String>,
    bind: Option<String>,
    port: Option<String>,
    auto_port: Option<bool>,
    insecure: bool,
    embed_mode: Option<String>,
    onnx_model: Option<String>,
    onnx_tokenizer: Option<String>,
    compat_distribution: Option<String>,
    compat_version: Option<String>,
    decide_mode: Option<String>,
    decide_model_dir: Option<String>,
}

/// Subcommands dispatched before the server-flag parser. A flag written
/// before one of these is moved behind it (`xerj --url URL corpus …`).
fn is_top_level_command(arg: &str) -> bool {
    matches!(
        arg,
        "index"
            | "search"
            | "batch-search"
            | "def"
            | "init"
            | "gain"
            | "autoindex"
            | "brain"
            | "share"
            | "feedback"
            | "memory"
            | "code"
            | "corpus"
            | "hub"
            | "ask"
            | "mcp"
            | "__extract-pdf"
            | xerj_autoindex::extract::xls::WORKER_ARG
    )
}

fn parse_args() -> CliArgs {
    parse_args_from(std::env::args().skip(1)).unwrap_or_else(|e| {
        eprintln!("error: {e}. Use --help for usage.");
        std::process::exit(2);
    })
}

/// The value that must follow `flag`. A flag given last on the line, or
/// followed straight by another `--flag`, has no value: that is an error, not
/// a silent "unset" — `xerj --insecure --data-dir` used to boot on the default
/// data directory, and `xerj --data-dir --insecure` used a directory literally
/// named `--insecure` and left auth on (#1435). The short forms are the same
/// trap — `xerj --data-dir -k` must not name the directory `-k` and drop
/// `--insecure` — so a single dash plus one letter counts as a flag too; any
/// longer single-dash value (`-data`, `-1`, `::1`) is still a value.
fn flag_value(flag: &str, next: Option<String>) -> Result<String, String> {
    match next {
        Some(v) if !looks_like_flag(&v) => Ok(v),
        Some(v) => Err(format!("{flag} needs a value, got the flag {v}")),
        None => Err(format!("{flag} needs a value")),
    }
}

/// `--anything`, or a short flag such as `-k` / `-d`.
fn looks_like_flag(v: &str) -> bool {
    v.starts_with("--") || matches!(v.as_bytes(), [b'-', c] if c.is_ascii_alphabetic())
}

/// The body of [`parse_args`], over any iterator of arguments — the same
/// testable-parse shape as `brain::parse` and `share::parse`, so the CLI
/// grammar is unit-tested without touching process-wide `argv` (which every
/// test in the same binary shares).
fn parse_args_from<I>(args: I) -> Result<CliArgs, String>
where
    I: Iterator<Item = String>,
{
    let mut args = args;
    let mut config = None;
    let mut data_dir = None;
    let mut bind = None;
    let mut port: Option<String> = None;
    let mut auto_port = None;
    let mut insecure = false;
    let mut embed_mode = None;
    let mut onnx_model = None;
    let mut onnx_tokenizer = None;
    let mut compat_distribution = None;
    let mut compat_version = None;
    let mut decide_mode = None;
    let mut decide_model_dir = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" | "-c" => {
                config = Some(PathBuf::from(flag_value(&arg, args.next())?));
            }
            "--data-dir" | "-d" => {
                data_dir = Some(flag_value(&arg, args.next())?);
            }
            "--bind" | "-b" => {
                bind = Some(flag_value(&arg, args.next())?);
            }
            "--port" => {
                port = Some(flag_value(&arg, args.next())?);
            }
            "--auto-port" => {
                auto_port = Some(true);
            }
            "--no-auto-port" => {
                auto_port = Some(false);
            }
            "--insecure" | "-k" => {
                insecure = true;
            }
            "--embed-mode" => {
                embed_mode = Some(flag_value(&arg, args.next())?);
            }
            "--onnx-model" => onnx_model = Some(flag_value(&arg, args.next())?),
            "--onnx-tokenizer" => onnx_tokenizer = Some(flag_value(&arg, args.next())?),
            "--compat-distribution" => compat_distribution = Some(flag_value(&arg, args.next())?),
            "--compat-version" => compat_version = Some(flag_value(&arg, args.next())?),
            "--decide-mode" => decide_mode = Some(flag_value(&arg, args.next())?),
            "--decide-model-dir" => decide_model_dir = Some(flag_value(&arg, args.next())?),
            // Read out of band by `xerj_common::feedback` (which scans the
            // whole argument list, so its position never matters); accepted
            // here only so it is not an "unknown argument".
            xerj_common::feedback::DISABLE_FLAG => {}
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--version" | "-V" => {
                println!("xerj v{}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            // A usage error exits 2 (the same code `parse_args` uses for a
            // missing flag value). Exiting 1 here made `xerj nosuch` disagree
            // with the documented "usage is 2" rule.
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    Ok(CliArgs {
        config,
        data_dir,
        bind,
        port,
        auto_port,
        insecure,
        embed_mode,
        onnx_model,
        onnx_tokenizer,
        compat_distribution,
        compat_version,
        decide_mode,
        decide_model_dir,
    })
}

fn print_help() {
    println!("{}", help_text(xerj_common::feedback::enabled()));
}

/// The help text as a value, so tests can assert that a documented behaviour is
/// still documented — including *where* the feedback invitation lands — instead
/// of trusting a `println!`. Same house style as `xerj_autoindex::cli::help_text`.
fn help_text(feedback: bool) -> String {
    format!(
        "xerj v{} — the unified search engine for AI (Elasticsearch wire-compatible)\n\
         \n\
         {}\
         USAGE:\n\
         \x20\x20\x20\x20A flag may precede the subcommand: xerj --url URL corpus index <name>\n\
         \x20\x20\x20\x20is read as xerj corpus index <name> --url URL.\n\
         \n\
         \x20\x20\x20\x20xerj [OPTIONS]                      start the server (options below)\n\
         \x20\x20\x20\x20xerj autoindex <folder> [OPTIONS]   zero-config discovery + indexing — the\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20flagship path for most agents (see xerj\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20autoindex --help)\n\
         \x20\x20\x20\x20xerj autoindex map                  print the discovered data map\n\
         \x20\x20\x20\x20xerj autoindex remove --prefix <P>  drop one autoindexed folder, after a plan\n\
         \x20\x20\x20\x20xerj code <corpus> \"<query>\"        reference-coding retrieval over a corpus\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20of peer projects (see xerj code --help)\n\
         \x20\x20\x20\x20xerj corpus add|index|list          the corpus lifecycle behind `xerj code`\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20(see xerj corpus --help)\n\
         {ask_lines}\
         \x20\x20\x20\x20xerj index [OPTIONS]                direct NDJSON → engine ingest (see xerj\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20index --help)\n\
         \x20\x20\x20\x20xerj brain <folder>                 index + browse in one command (see xerj\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20brain --help)\n\
         \x20\x20\x20\x20xerj share <index|folder>           give someone read-only search over one\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20index: link + passcode + expiry (see xerj\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20share --help)\n\
         \x20\x20\x20\x20xerj feedback [OPTIONS]             draft or --submit the agent field report —\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20see xerj feedback --help\n\
         \n\
         OPTIONS (for `xerj [OPTIONS]`, the bare server start):\n\
         \x20\x20\x20\x20--config,   -c <PATH>  Path to TOML config file. CORS is off by default (no\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Access-Control-* headers); a browser app calling the node\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20directly needs [cors] allowed_origins =\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20[\"http://localhost:8080\"] there (allow_any_origin = true\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20is for local development only)\n\
         \x20\x20\x20\x20--data-dir, -d <PATH>  Override data directory\n\
         \x20\x20\x20\x20--bind,     -b <ADDR>  Interface to bind every listener to. Default 127.0.0.1 —\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20loopback only, reachable from this machine and nowhere\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20else. Pass 0.0.0.0 (or a specific address) to expose the\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20node; with TLS off that also needs\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20server.allow_insecure_network_bind = true (or\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20XERJ_ALLOW_INSECURE_NETWORK_BIND=true), because every\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20request — API key included — would cross the network in\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20cleartext. Env: XERJ_BIND_ADDRESS\n\
         \x20\x20\x20\x20--insecure, -k         Disable TLS *and* API-key authentication — local dev only.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Auth is ON by default, so without this flag every client\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20(`xerj autoindex` and `xerj brain` included) must send the\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20key this server writes to <data_dir>/admin.key on first\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20start. The default data directory is ./data. Keep it outside any folder\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20you will autoindex — ./data inside that tree gets indexed, because data/\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20is not in the built-in ignore list. Start it with xerj --data-dir ~/xerj-data\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20(Windows: $HOME\\xerj-data), then:\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20export XERJ_API_KEY=\"$(cat ~/xerj-data/admin.key)\"\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20On Windows PowerShell:\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20$env:XERJ_API_KEY = Get-Content \"$env:USERPROFILE\\xerj-data\\admin.key\"\n\
         \x20\x20\x20\x20--embed-mode <MODE>    Embedding backend: lexical | neural | proxy | auto |\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20onnx-experimental\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20lexical  built-in feature-hash (default; offline, not neural)\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20neural   built-in BERT (all-MiniLM-L6-v2) — real semantics,\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20in-process. The model (~90 MB) loads at startup:\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20downloaded from the HuggingFace Hub, or read from\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20embedding.local_model_dir, then cached. If that load\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20fails, the process refuses to start. It does not keep\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20running and fail every ingest, and it does not write\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20lexical vectors. Restart with: xerj --embed-mode lexical\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Or set HF_ENDPOINT to a Hub mirror and start again.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20The mode is fixed per index at ingest time. Documents keep\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20the vectors written when they were indexed. Changing\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20--embed-mode does not re-embed an existing index; re-index.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Lexical is the fast default.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20proxy    external OpenAI-compatible embeddings endpoint.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20embedding.default_endpoint may be a /v1 base, a bare host,\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20or the full /v1/embeddings URL (a trailing slash is ignored).\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Any other path refuses to start and names the URL to set.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20auto     proxy if embedding.default_endpoint is set, else lexical\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20onnx-experimental  local all-MiniLM-L6-v2-compatible FP32\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20ONNX prototype; width 384, named BERT inputs and\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20token output; requires matching tokenizer.json;\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20no auto-download\n\
         \x20\x20\x20\x20--onnx-model <PATH>    compatible FP32 MiniLM ONNX model\n\
         \x20\x20\x20\x20--onnx-tokenizer <PATH> tokenizer.json from the same model/export\n\
         \x20\x20\x20\x20--compat-distribution <DIST>  Force the wire-compat client identity: elasticsearch |\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20opensearch. Default: unset — xerj auto-detects per request\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20from the caller's User-Agent header instead (an\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20opensearch-py/opensearch-js client — including OpenSearch\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Dashboards itself — and an elasticsearch-py/elasticsearch-js\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20client hitting the SAME running instance each see GET / and\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20GET /_nodes shaped for their own ecosystem). Setting this\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20pins ALL clients to one identity regardless of User-Agent —\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20useful if a client sends no recognizable User-Agent, or you\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20just want deterministic behavior.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Example: --compat-distribution opensearch\n\
         \x20\x20\x20\x20--compat-version <VER> Force the reported version.number (e.g. 2.11.0). Default:\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20unset — when auto-detected as OpenSearch, reports a fixed\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20version verified to pass OpenSearch Dashboards' own\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20compatibility check (not the calling client's own version —\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20client library versions aren't a reliable stand-in for a\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20compatible server version, confirmed against a real OSD\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20container). Example: --compat-version 2.11.0\n\
         \x20\x20\x20\x20--decide-mode <MODE>   System One decide ladder (/v1/systemone, /_decide):\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20history | local (default history — the labelled-history\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20vote, today's contract). `local` arms the tier-2 zero-shot\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20decision head, which answers the questions the history\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20vote cannot (no [decisions] index, no labelled neighbour)\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20instead of 503/422. Needs --decide-model-dir (or\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20XERJ_DECIDE_MODEL_DIR) naming a directory with\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20config.json, tokenizer.json and model.safetensors, and a\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20build with the decide-local feature\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20(cargo build --release -p xerj-server --features\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20decide-local). Env: XERJ_DECIDE_MODE\n\
         \x20\x20\x20\x20--decide-model-dir <PATH>  Directory holding the tier-2 decide head's\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20config.json, tokenizer.json and model.safetensors —\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20local file only, no download path. Env:\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20XERJ_DECIDE_MODEL_DIR\n\
         \x20\x20\x20\x20--port      <PORT>     Port for the Elasticsearch-compatible API (default 9200).\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20When given, also claims PORT+1 for the native REST API and\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20PORT+2 for gRPC, so a second instance needs only this one\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20flag: xerj --insecure --port 9300 -d ./other-data\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Without it the native REST API listens on 8080 and gRPC on\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x208081 ([server] rest_port / grpc_port in the TOML config).\n\
         \x20\x20\x20\x20--auto-port            Opt in to multi-instance mode: if a port is taken, bind the\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20next free one; if the data dir is held by another live\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20process, use <data-dir>-2, -3, ... The banner prints the\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20ports actually bound. Off by default. Env: XERJ_AUTO_PORT\n\
         \x20\x20\x20\x20--no-auto-port         Fail at startup if a port is taken (the default).\n\
         \x20\x20\x20\x20--disable-feedback     Do not print the feedback invitation above. Honoured in any\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20position, including after --help. Env:\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20XERJ_DISABLE_FEEDBACK=true\n\
         \x20\x20\x20\x20--help,     -h         Show this help\n\
         \x20\x20\x20\x20--version,  -V         Print version and exit\n\
         \n\
         SUBCOMMANDS:\n\
         \x20\x20\x20\x20xerj batch-search --input FILE  ordered batch retrieval, JSON in/out (see xerj batch-search --help)\n\
         \x20\x20\x20\x20xerj index      <opts>          direct NDJSON → engine ingest (see xerj index --help)\n\
         \x20\x20\x20\x20xerj autoindex  <folder> [opts] zero-config folder discovery + indexing (see xerj autoindex --help)\n\
         \x20\x20\x20\x20xerj autoindex  map             print the discovered data map\n\
         \x20\x20\x20\x20xerj search     \"<text>\"         retrieve code/passages from a running node — the\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20query client, no wrapper scripts (see xerj search --help)\n\
         \x20\x20\x20\x20xerj def        \"<symbol>\"       go-to-definition: where is this symbol defined, and\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20what is its signature (see xerj def --help)\n\
         \x20\x20\x20\x20xerj code       <corpus> \"<q>\"  reference-coding retrieval: how did peer projects\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20solve this (see xerj code --help)\n\
         \x20\x20\x20\x20xerj corpus     add|index|list   clone + index + list the reference corpora that\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20`xerj code` searches (see xerj corpus --help)\n\
         \x20\x20\x20\x20xerj init                       wire XERJ into the coding agents in this project:\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20MCP + skill files, one command (see xerj init --help)\n\
         \x20\x20\x20\x20xerj gain                       what this node did for you, counted from the audit\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20log — searches, hit rate, latency (xerj gain --help)\n\
         \x20\x20\x20\x20xerj brain      <folder>        one command: index a folder into a running, browsable\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20second brain in your browser (see xerj brain --help)\n\
         \x20\x20\x20\x20xerj memory     store <ns> --text <t>\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20store one agent memory on a running node\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20(see xerj memory --help)\n\
         \x20\x20\x20\x20xerj mcp        [opts]          Model Context Protocol stdio server: exposes {mcp_tools} tools\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20(search, semantic, vector, hybrid, memory, second-brain,\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20map, plan, code-search, Corpus Hub) to any MCP client.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Proxies to a node you already started — set XERJ_URL or\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20--url (see xerj mcp --help)\n\
         \x20\x20\x20\x20xerj feedback   [opts]          draft the agent field report XERJ asks every agent to\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20file; --submit sends it (GitHub PR first, API fallback),\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20--dry-run just prints (see xerj feedback --help)\n\
         \n\
         SEARCHING A CODEBASE:\n\
         \x20\x20\x20\x20A search hit returns the whole indexed document, which for source code is the\n\
         \x20\x20\x20\x20whole file. To get only the matching function or class, ask for the passage:\n\
         \n\
         \x20\x20\x20\x20\x20\x20\x20\x20curl -s $URL/<index>/_search -H 'Content-Type: application/json' \\\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20-d '{{\"query\":{{\"bool\":{{\"should\":[{{\"multi_match\":{{\"query\":\"<symbol or phrase>\",\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\"fields\":[\"body\",\"defs\"],\"type\":\"most_fields\"}}}},{{\"match_phrase\":{{\"defs\":{{\"query\":\"<symbol or phrase>\",\"boost\":4}}}}}}]}}}},\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\"_source\":[\"ax_path\",\"title\"],\"fields\":[\"_passage\"]}}'\n\
         \n\
         \x20\x20\x20\x20the `match_phrase` clause ranks the file that DEFINES a symbol above files that merely call it;\n\
         \x20\x20\x20\x20`_passage` returns the enclosing block. Without them you get whole files.\n\
         \n\
         ENVIRONMENT:\n\
         \x20\x20\x20\x20XERJ_LOG         Log level filter (default: info)\n\
         \x20\x20\x20\x20XERJ_DISABLE_FEEDBACK  true/false — same as --disable-feedback, for callers that\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20cannot change the command line (CI, containers, agents)\n\
         \x20\x20\x20\x20XERJ_CONFIG      Config file path\n\
         \x20\x20\x20\x20XERJ_EMBED_MODE  Embedding backend (lexical|neural|proxy|auto|onnx-experimental)\n\
         \x20\x20\x20\x20XERJ_ONNX_MODEL  FP32 ONNX model path\n\
         \x20\x20\x20\x20XERJ_ONNX_TOKENIZER matching tokenizer.json path\n\
         \x20\x20\x20\x20XERJ_COMPAT_DISTRIBUTION  elasticsearch|opensearch — same as --compat-distribution\n\
         \x20\x20\x20\x20XERJ_COMPAT_VERSION       same as --compat-version\n\
         \x20\x20\x20\x20XERJ_DECIDE_MODE          decide ladder: history|local — same as --decide-mode\n\
         \x20\x20\x20\x20XERJ_DECIDE_MODEL_DIR     tier-2 decide head's model directory — same as\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20--decide-model-dir\n\
         \x20\x20\x20\x20XERJ_INGEST_MEMORY_TRACE  off|summary (default: off); summary emits bounded\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20periodic NDJSON diagnostics, never per-item events\n\
         \x20\x20\x20\x20XERJ_INGEST_MEMORY_SAMPLE_MS  sampler period, clamped to 25..60000 ms\n\
         \x20\x20\x20\x20XERJ_INGEST_MEMORY_OUTPUT tracing|<PATH> (default: tracing); append NDJSON to path\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Merge/cache fields are reserved/unavailable in v1.\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20Active+drained may transiently overlap across sampling.\n\
         \n\
         WIRE-COMPAT IDENTITY — three ways to run this, pick what fits:\n\
         \n\
         \x20\x20\x20\x201. Fully automatic (default, no flags):\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20xerj -d ./data\n\
         \x20\x20\x20\x20\x20\x20\x20Every client gets a response shaped for its own ecosystem, detected from its\n\
         \x20\x20\x20\x20\x20\x20\x20User-Agent on every request. One instance serves ES and OpenSearch clients at\n\
         \x20\x20\x20\x20\x20\x20\x20once.\n\
         \n\
         \x20\x20\x20\x202. Force one identity for every client, every request:\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20xerj -d ./data --compat-distribution opensearch --compat-version 2.11.0\n\
         \x20\x20\x20\x20\x20\x20\x20Or via env (handy in docker-compose):\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20XERJ_COMPAT_DISTRIBUTION=opensearch XERJ_COMPAT_VERSION=2.11.0 xerj -d ./data\n\
         \x20\x20\x20\x20\x20\x20\x20Ignores User-Agent entirely — useful for a client that doesn't send one, or when\n\
         \x20\x20\x20\x20\x20\x20\x20you want fully deterministic behavior.\n\
         \n\
         \x20\x20\x20\x203. Pin only the distribution, let the version auto-detect per client:\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20xerj -d ./data --compat-distribution opensearch\n\
         \x20\x20\x20\x20\x20\x20\x20(or set only XERJ_COMPAT_VERSION and leave distribution to auto-detect — the two\n\
         \x20\x20\x20\x20\x20\x20\x20settings are resolved independently, mix and match as needed.)\n",
        env!("CARGO_PKG_VERSION"),
        xerj_common::feedback::block(feedback),
        // Counted from the served `tools/list`, not typed: the literal "13"
        // went stale the day the three Corpus Hub tools landed (#1443).
        mcp_tools = mcp_tool_count(),
        ask_lines = ask_help_lines(xerj_common::hub::hub_key_configured()),
    )
}

/// The `xerj ask` usage lines, and — when this build can verify a hub pack —
/// the `xerj hub` line beside them. A build with no hub signing key can verify
/// no pack, so there the ask line says the command is unavailable instead of
/// promising a pull that will only ever exit 5, and `xerj hub` is left off:
/// that build cannot start the node `xerj hub stop` would stop.
fn ask_help_lines(hub: bool) -> &'static str {
    // A trailing `\` eats the next line's source indent. The four spaces on
    // the command, and the wrap under the description, are `\x20` so they
    // survive. Descriptions in this block start at column 40, same as the
    // other USAGE lines.
    if hub {
        "\x20\x20\x20\x20xerj ask \"<question>\"               answer a question from a hub corpus: the\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20pre-indexed pack is pulled the first time\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20(see xerj ask --help); `xerj \"<question>\"`\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20is the same. `xerj hub stop` stops the\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20node xerj ask started\n\
         \x20\x20\x20\x20xerj hub <command>                  pull, find, stop, ... see xerj hub --help\n"
    } else {
        "\x20\x20\x20\x20xerj ask \"<question>\"              Corpus Hub answers: not available in this\n\
         \x20\x20\x20\x20build (no hub signing key; exits 5).\n\
         \x20\x20\x20\x20A folder you autoindexed: xerj search / xerj def.\n\
         \x20\x20\x20\x20Public libraries: xerj corpus + xerj code\n"
    }
}

/// How many tools `xerj mcp` serves — the length of the same `tool_specs()`
/// array its `tools/list` returns.
#[cfg(test)]
mod ask_help_tests {
    use super::ask_help_lines;

    #[test]
    fn a_build_without_a_hub_key_does_not_send_ask_at_the_local_index() {
        let lines = ask_help_lines(false);
        assert!(lines.contains("exits 5"), "{lines}");
        assert!(lines.contains("xerj search"), "{lines}");
        assert!(lines.contains("xerj def"), "{lines}");
        assert!(lines.contains("xerj corpus"), "{lines}");
        assert!(
            ask_help_lines(true).contains("xerj ask"),
            "a build with a key still documents the command"
        );
    }
}

fn mcp_tool_count() -> usize {
    xerj_mcp::tool_specs().as_array().map_or(0, Vec::len)
}

// ─────────────────────────────────────────────────────────────────────────────
// Startup banner
// ─────────────────────────────────────────────────────────────────────────────

/// Why a bind address could not be turned into a listener. One message for all
/// three listeners: they share the one setting, so three different phrasings of
/// the same fault would only obscure it.
fn bad_bind_address(cfg: &Config) -> String {
    format!(
        "server.bind_address = {:?} is not an IP address — it must be an IPv4 or \
         IPv6 literal such as \"127.0.0.1\" (the default), \"0.0.0.0\" or \"::1\". \
         Host names are not resolved.",
        cfg.server.bind_address
    )
}

/// `bind_address` in a form that can be pasted into a URL: IPv6 literals need
/// brackets (`http://[::1]:9200`), IPv4 and host names do not.
fn url_host(bind: &str) -> String {
    match bind.trim().parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V6(v6)) => format!("[{v6}]"),
        _ => bind.trim().to_string(),
    }
}

/// The three addresses the node actually holds, as reported by the kernel.
///
/// The banner is printed from *these*, never from `cfg.server.*_port`, because
/// a port in the config is a request and a port in here is a fact. See
/// step 10a of `async_main` for why that distinction is load-bearing (#465).
struct BoundAddrs {
    rest: SocketAddr,
    es: SocketAddr,
    grpc: SocketAddr,
}

/// The auth-disabled posture line. On loopback the bind line says nothing
/// off this host can reach the listeners, so this line must not then say
/// the network can write.
fn auth_disabled_line(loopback: bool) -> &'static str {
    if loopback {
        " │ ⚠  Auth:   DISABLED (--insecure) — every caller on this machine can write"
    } else {
        " │ ⚠  Auth:   DISABLED (--insecure) — anyone on the network can write"
    }
}

fn print_banner(cfg: &Config, bound: &BoundAddrs, startup_ms: u128) {
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
    // The bind address belongs on every listener line (issue #228). Printing
    // bare `:9200` left the one fact that decides who can reach this node off
    // the only screen most operators ever read, and made a loopback-bound node
    // and a world-bound node look identical.
    // Bracketed for IPv6 so `host:port` stays unambiguous.
    let bind_hostport = url_host(&cfg.server.bind_address);
    let bind = bind_hostport.as_str();
    println!(" Native REST  {}:{} [{}]", bind, bound.rest.port(), tls);
    println!(" ES-compat    {}:{} [{}]", bind, bound.es.port(), tls);
    // Never interpolated with `tls`: the gRPC listener is h2c whatever
    // `[tls]` says (issue #229), and a line that read "TLS" here would be the
    // exact false promise the startup check exists to prevent.
    println!(
        " gRPC         {}:{} [h2c — plaintext, no TLS]",
        bind,
        bound.grpc.port()
    );
    println!(" Data dir     {}", cfg.server.data_dir);
    println!(" Started in   {}ms", startup_ms);
    println!();
    println!(
        " Xerj Console UI    http://localhost:{}/_xerj-console/  ({} files bundled)",
        bound.es.port(),
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
    // Reach, before anything else: it decides whether the rest of this block
    // is a local-development note or a production exposure (issue #228).
    if cfg.bind_address_is_loopback() {
        println!(" │ ✓  Bind:   {bind} — loopback only; nothing off this host can reach the");
        println!(" │           listeners. Pass --bind 0.0.0.0 to expose the node.");
    } else if !cfg.tls.enabled {
        // Reaching here means the operator set the opt-out, so say what it
        // bought them rather than printing a generic warning.
        println!(" │ ⚠  Bind:   {bind} — network-reachable AND TLS is off. API keys and");
        println!(" │           document bodies cross the network in cleartext, allowed by");
        println!(" │           server.allow_insecure_network_bind (issue #228). Terminate");
        println!(" │           TLS in front of this node, or bind loopback.");
    } else {
        println!(" │ ⚠  Bind:   {bind} — network-reachable on every listed port.");
    }
    if !cfg.tls.enabled {
        println!(" │ ⚠  TLS:    listener is plain TCP — terminate TLS at a reverse proxy");
        println!(" │           (or enable in-process TLS: tls.enabled = true)");
    } else {
        println!(" │ ✓  TLS:    in-process rustls termination active (REST + ES-compat)");
        println!(" │           (self-signed by default — supply a CA cert for production)");
        // The exposure the startup check refuses by default. Reaching here
        // with a non-loopback bind means the operator set the opt-out, so
        // say what it bought them rather than repeating the generic ✓.
        if !cfg.bind_address_is_loopback() {
            println!(
                " │ ⚠  gRPC:   :{} is NOT covered by TLS — cleartext h2c on a non-loopback",
                bound.grpc.port()
            );
            println!(" │           bind, allowed by tls.allow_insecure_grpc_h2c (issue #229).");
            println!(" │           Terminate TLS for it at your proxy/mesh, or bind loopback.");
        } else {
            println!(
                " │ ⚠  gRPC:   :{} is NOT covered by TLS — cleartext h2c, loopback-only",
                bound.grpc.port()
            );
        }
    }
    if !cfg.auth.enabled {
        // The bind line above already said whether anything off this host can
        // connect. Repeating "anyone on the network" on a loopback node
        // contradicts that line.
        println!("{}", auth_disabled_line(cfg.bind_address_is_loopback()));
    } else {
        println!(
            " │ ✓  Auth:   single API-key (no RBAC; per-doc / per-field controls roadmap v0.9)"
        );
    }
    // Issue #201 made the hash chain durable; issue #329 gave it write
    // coverage, real subjects and a privilege gate. What is left to warn about
    // is the window: it is a bounded ring, so "no entry" past the window is not
    // evidence of no write. Say exactly that, and no more than that.
    println!(
        " │ ✓  Audit:  hash-chained log of writes + searches + API-key ops, kept in \
         <data_dir>/audit.jsonl"
    );
    println!(
        " │           (last {} entries, one per request; /_audit/* requires read_audit)",
        xerj_engine::audit::DEFAULT_AUDIT_CAPACITY
    );
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

    // `rerank.endpoint` is where document text gets POSTed. `Config::validate`
    // checked the file's value; the `TYPESAFE_ENDPOINT` fallback is only
    // known here, at boot, and gets the same two rules — the docs promise the
    // node refuses to start on a credentialed or non-http endpoint, whichever
    // place it came from.
    if let Err(reason) = cfg.rerank.check_environment() {
        anyhow::bail!("config error: {reason}");
    }

    // Bind address override: `--bind` flag or `XERJ_BIND_ADDRESS` env (flag
    // wins). Both exist because the default is loopback (#228): without a way
    // to say "expose me" that is not a TOML file, a container image or a
    // one-off `xerj -b 0.0.0.0` would have no path at all.
    if let Some(addr) = args
        .bind
        .clone()
        .or_else(|| std::env::var("XERJ_BIND_ADDRESS").ok())
    {
        let addr = addr.trim().to_string();
        if addr.is_empty() {
            anyhow::bail!("--bind / XERJ_BIND_ADDRESS is empty; pass an IP address");
        }
        info!("server.bind_address = {addr} (from CLI/env)");
        cfg.server.bind_address = addr;
    }

    // Running a second instance used to require hand-writing a TOML file, and
    // the three port keys are not mentioned in `--help`. In a blind usability
    // run an agent tried `-b 127.0.0.1:9310`, got "not an IP address", and
    // ended up running `strings` on the binary to find the config key names.
    // One flag removes that entirely. The sibling ports are derived so a second
    // instance does not collide on them either; set them individually in a
    // config file when that matters.
    if let Some(p) = args.port.clone() {
        let base: u16 = p.parse().map_err(|_| {
            anyhow::anyhow!("--port must be a number between 1 and 65533, got {p:?}")
        })?;
        if base == 0 || base > 65_533 {
            anyhow::bail!(
                "--port must be between 1 and 65533 (it also claims PORT+1 for the \
                 native REST API and PORT+2 for gRPC), got {base}"
            );
        }
        info!(
            "server.es_compat_port = {base}, rest_port = {}, grpc_port = {} (from --port)",
            base + 1,
            base + 2
        );
        cfg.server.es_compat_port = base;
        cfg.server.rest_port = base + 1;
        cfg.server.grpc_port = base + 2;
    }

    // Multi-instance auto-port selection: when enabled, if the requested
    // listener ports are already in use, the server scans for and binds the
    // next free ports, and moves off a data dir another live process holds.
    //
    // OPT-IN ONLY (flag, env, or `[server] auto_port = true`). A taken port is
    // a fatal startup error by default on purpose (issue #465): a client
    // pointed at the requested port would otherwise talk to — and write into —
    // whichever other instance owns it, while this one reports success on a
    // port nobody is addressing. Precedence: CLI flag > env > config file.
    if let Some(ap) = args.auto_port {
        cfg.server.auto_port = ap;
    } else if let Ok(raw) = std::env::var("XERJ_AUTO_PORT") {
        // Parsed strictly, like XERJ_ALLOW_INSECURE_NETWORK_BIND: a typo must
        // not silently pick a behaviour the operator did not ask for.
        cfg.server.auto_port = match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => anyhow::bail!("XERJ_AUTO_PORT={raw:?} is not a boolean; use true or false"),
        };
    }

    if cfg.server.auto_port {
        cfg.server.data_dir = resolve_available_data_dir(&cfg.server.data_dir);
    }

    // The #228 opt-out, also settable from the environment so a container can
    // declare its exposure without shipping a config file. Parsed strictly: a
    // value we cannot read is refused rather than treated as `false`, because
    // silently falling back to the safe value here means a boot that refuses
    // with a message about a setting the operator believes they set.
    if let Ok(raw) = std::env::var("XERJ_ALLOW_INSECURE_NETWORK_BIND") {
        let v = raw.trim();
        let parsed = match v.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => anyhow::bail!(
                "XERJ_ALLOW_INSECURE_NETWORK_BIND={raw:?} is not a boolean; \
                 use true or false"
            ),
        };
        info!("server.allow_insecure_network_bind = {parsed} (from env)");
        cfg.server.allow_insecure_network_bind = parsed;
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

    // #1189: an explicit mode=proxy is validated at boot, the same
    // fail-closed contract as onnx-experimental above. The old warn-plus-
    // lexical fallback started a "healthy" node whose embedding requests
    // silently wrote lexical vectors. Auto keeps its documented fallback.
    if cfg.embedding.mode.trim().eq_ignore_ascii_case("proxy") {
        if cfg.embedding.default_endpoint.trim().is_empty() {
            anyhow::bail!(
                "embedding.mode=proxy requires embedding.default_endpoint to be set; \
                 remove mode=proxy (use auto) if a lexical fallback is wanted"
            );
        }
        match xerj_ai::embed::EmbeddingProxy::new(xerj_engine::index::proxy_config(&cfg.embedding))
        {
            Ok(proxy) => {
                info!(
                    endpoint = %proxy.request_url(),
                    configured = %cfg.embedding.default_endpoint,
                    "embedding.mode=proxy request URL"
                );
            }
            Err(e) => {
                anyhow::bail!("embedding.mode=proxy failed to initialize: {e}");
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

    // Persist before announcing. The notice names the path and never the
    // secret: stdout is what agents capture, and a first-run banner that
    // prints the key lands in those logs (field report 2026-10-17).
    if let Err(e) = std::fs::create_dir_all(data_dir).and_then(|_| {
        // 0600 — this file IS the admin credential (RC4 W2 #21).
        write_secret_file(&key_path, key.as_bytes())
    }) {
        warn!(
            "could not persist admin key to {}: {e}. The key was not printed. \
             Fix the data directory and restart so a key can be written.",
            key_path.display()
        );
    } else {
        println!("{}", first_run_admin_key_notice(&key, &key_path));
    }

    cfg.auth.admin_api_key = key;
    Ok(())
}

/// Text printed once, when an admin key is minted.
///
/// `key` is accepted so a test can prove the rendered text does not contain
/// it. The function refuses to return a notice that includes the secret.
fn first_run_admin_key_notice(key: &str, key_path: &Path) -> String {
    let path = key_path.display().to_string();
    let notice = format!(
        "\n\
         First-run: admin API key auto-generated.\n\
         The key is not printed here. Read it from (owner-only, mode 0600):\n\
         {path}\n\
         Keep that file secret. Clients send it as `Authorization: ApiKey $(cat {path})`.\n"
    );
    assert!(
        !key.is_empty() && !notice.contains(key),
        "first-run notice must not contain the admin key"
    );
    notice
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

/// Settings this build accepts but does not act on. An operator who throttles
/// merges to protect query latency was getting no throttle and no signal
/// (#207); the signal is the minimum. See `MergeConfig::dormant_overrides` for
/// why these are a warning rather than the hard startup error `storage.backend`
/// gets. `[compression]` (#318) and the `[fts]` / `[vector]` / `[logs]` /
/// `[indexing]` / `storage.local_cache_dir` keys that nothing reads get the
/// same treatment — `Config::dormant_overrides` collects every section.
///
/// Call it after `init_tracing`. It used to run inside `load_config`, which
/// runs before the subscriber exists, so every one of these warnings was
/// dropped and an operator saw nothing.
fn warn_dormant_overrides(cfg: &Config) {
    for (key, effect) in cfg.dormant_overrides() {
        warn!("{key} is set but has no effect in this build: {effect}");
    }
}

/// `--embed-mode` (and `embedding.mode`) is an ingest-time property of every
/// document. `auto` is the default and stays quiet; an explicit mode is the
/// case that reads as a cheap switch.
fn embed_mode_fixed_at_ingest_warning(mode: &str) -> Option<String> {
    let trimmed = mode.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("auto") {
        return None;
    }
    Some(format!(
        "embedding.mode={trimmed} is fixed per index at ingest time. Documents keep the vectors \
         written when they were indexed. Changing --embed-mode does not re-embed an existing \
         index; re-index into a new index. The fast default is lexical (`xerj --embed-mode lexical`, \
         or omit the flag)."
    ))
}

fn warn_embed_mode_is_fixed_at_ingest(cfg: &Config) {
    if let Some(message) = embed_mode_fixed_at_ingest_warning(&cfg.embedding.mode) {
        warn!("{message}");
    }
}

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

    // Awaited once, from `async_main`. The listeners wait on `ShutdownPhase`
    // instead, so they do not close their ports before the final flush. Log
    // the banner only once.
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
/// Take an address and bind it, then serve. Kept for tests, which want the
/// convenience of `:0` and do not care about startup ordering.
///
/// The server itself must NOT use this: it binds every listener up front at
/// step 10a of `async_main`, so that a taken port is a startup failure rather
/// than a line in the log under a banner that already claimed success
/// (issue #465).
#[cfg(test)]
async fn serve(
    router: Router,
    addr: SocketAddr,
    name: &'static str,
    tls: Option<RustlsConfig>,
) -> Result<()> {
    // Unit tests stop the server by dropping the runtime, not by signalling.
    // Holding the sender keeps the phase at `Serving` for the life of `serve`.
    let (tx, rx) = tokio::sync::watch::channel(ShutdownPhase::Serving);
    let _hold = tx;
    serve_on(router, bind_listener(addr, name)?, name, tls, rx).await
}

/// How far shutdown has gotten.
///
/// Listeners used to stop accepting on the signal itself, and the final flush
/// ran only after those tasks returned. `/health/ready` then refused
/// connections while `flush_all_force` was still publishing segments. A caller
/// that treats connection refusal as "the process is done with the data
/// directory" — the use-case harness wipes the data dir at that moment —
/// deleted those segments out from under the flush (`Directory not empty`,
/// then `Segment … not found` on the next open).
///
/// `Draining` rejects new HTTP work and stops gRPC, but the HTTP ports stay
/// bound so a connect still succeeds. `Released` is after the flush; only
/// then do the HTTP listeners close.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ShutdownPhase {
    Serving,
    Draining,
    Released,
}

/// New HTTP requests are refused once shutdown starts, and counted until they
/// finish, so the final flush does not race a request that was allowed in.
/// `/health/live` stays 200: a liveness 5xx restarts the process (see
/// `native::liveness`).
#[derive(Clone)]
struct DrainGate {
    draining: Arc<AtomicBool>,
    in_flight: Arc<AtomicUsize>,
}

impl DrainGate {
    fn new() -> Self {
        Self {
            draining: Arc::new(AtomicBool::new(false)),
            in_flight: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn begin(&self) {
        self.draining.store(true, Ordering::SeqCst);
    }

    fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }
}

struct InFlightGuard(Arc<AtomicUsize>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn hold_port_while_draining(router: Router, gate: DrainGate) -> Router {
    router.layer(axum::middleware::from_fn(move |req, next| {
        let gate = gate.clone();
        async move { reject_when_draining(req, next, gate).await }
    }))
}

async fn reject_when_draining(req: Request, next: Next, gate: DrainGate) -> Response {
    // Liveness must keep answering 200 for the whole process lifetime. A 503
    // here is what makes kubelet kill the pod, which would cut the flush off.
    if req.uri().path() == "/health/live" {
        return next.run(req).await;
    }
    if gate.draining.load(Ordering::SeqCst) {
        return shutting_down_response();
    }
    gate.in_flight.fetch_add(1, Ordering::SeqCst);
    let _guard = InFlightGuard(Arc::clone(&gate.in_flight));
    if gate.draining.load(Ordering::SeqCst) {
        return shutting_down_response();
    }
    next.run(req).await
}

fn shutting_down_response() -> Response {
    (axum::http::StatusCode::SERVICE_UNAVAILABLE, "shutting down").into_response()
}

async fn wait_for_phase(
    mut rx: tokio::sync::watch::Receiver<ShutdownPhase>,
    ready: impl Fn(ShutdownPhase) -> bool,
) {
    loop {
        if ready(*rx.borrow()) {
            return;
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

/// Bind one listener, or fail.
///
/// Binding is synchronous and eager on purpose. The three listeners are bound
/// before the banner prints and before anything reports readiness, so the node
/// can never advertise a port another process owns — the failure mode that
/// sent one user's contracts, invoices and bank export into a stranger's data
/// directory under `ok=true exit=0` (issue #465).
/// The `[server]` config key that moves only this listener.
fn listener_port_key(name: &str) -> &'static str {
    match name {
        "ES-compat" => "es_compat_port",
        "native REST" => "rest_port",
        "gRPC" => "grpc_port",
        _ => "es_compat_port / rest_port / grpc_port",
    }
}

/// What startup prints when a listener cannot be bound.
///
/// Always `{name}: bind {addr}: {OS error}` first — the listener, the address
/// and the reason, which is what the #465 process test pins — then, for the
/// two failures an operator fixes by moving the port, the flags that move it.
///
/// #1687 added a hint for `AddrInUse` only, and #1778 re-appended the OS
/// reason after it. Two gaps remained. The hint told every listener to "pass
/// --port <PORT> to pick a different one", but `--port` sets the ES-compat
/// port and puts native REST on PORT+1 and gRPC on PORT+2, so for those two
/// it read as if `--port` were their own port. And the Windows refusal from
/// #1723 / #1738 (`os error 10013`, WSAEACCES — usually a port inside a
/// Hyper-V/WinNAT excluded range) is `PermissionDenied`, not `AddrInUse`, so
/// it still printed the bare `native REST: bind 127.0.0.1:8080` with no hint.
fn bind_error_message(name: &str, addr: SocketAddr, err: &std::io::Error) -> String {
    let key = listener_port_key(name);
    let move_it = "--port <BASE> moves all three listeners together (ES-compat BASE, \
         native REST BASE+1, gRPC BASE+2; e.g. --port 9300)";
    let only_this =
        format!("or set `{key}` under [server] in a --config file to move only this one");
    let not_a_server =
        "If you did not mean to start a server, `xerj --help` lists the subcommands.";
    match err.kind() {
        std::io::ErrorKind::AddrInUse => format!(
            "{name}: bind {addr}: {err}. Another process is already listening there. \
             {move_it}, --auto-port takes the next free ports, {only_this}. {not_a_server}"
        ),
        std::io::ErrorKind::PermissionDenied => {
            // `--auto-port` only steps past ports that are in use, so it is
            // not offered for a refused one.
            let cause = if cfg!(windows) {
                "Windows refused the port; it is often inside a range reserved by \
                 Hyper-V or WinNAT (list them with \
                 `netsh int ipv4 show excludedportrange protocol=tcp`)."
            } else if addr.port() < 1024 {
                "Ports below 1024 need elevated privileges."
            } else {
                "The operating system refused the port."
            };
            format!("{name}: bind {addr}: {err}. {cause} {move_it}, {only_this}. {not_a_server}")
        }
        _ => format!("{name}: bind {addr}: {err}"),
    }
}

fn bind_listener(addr: SocketAddr, name: &str) -> Result<std::net::TcpListener> {
    let listener = std::net::TcpListener::bind(addr)
        .map_err(|e| anyhow::anyhow!("{}", bind_error_message(name, addr, &e)))?;
    // Both axum's accept loop and axum-server's rustls acceptor drive this
    // listener from the reactor; a blocking fd would stall the runtime thread
    // on the first accept.
    listener
        .set_nonblocking(true)
        .with_context(|| format!("{name}: set the listener on {addr} non-blocking"))?;
    Ok(listener)
}

#[cfg(test)]
mod bind_listener_tests {
    use super::{bind_error_message, bind_listener};
    use std::io::{Error, ErrorKind};
    use std::net::SocketAddr;

    #[test]
    fn a_taken_port_names_port_and_auto_port() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a free port");
        let addr = held.local_addr().expect("read the held address");
        let err = bind_listener(addr, "ES-compat").expect_err("the port is held");
        let msg = format!("{err:#}");
        assert!(msg.contains("--port"), "{msg}");
        assert!(msg.contains("--auto-port"), "{msg}");
        assert!(msg.contains("xerj --help"), "{msg}");
        // #465's contract: the listener, the address, and the OS reason.
        assert!(msg.contains(&format!("ES-compat: bind {addr}: ")), "{msg}");
        assert!(msg.contains("os error"), "the OS error is kept: {msg}");
        assert!(msg.contains("`es_compat_port`"), "{msg}");
    }

    /// `--port` is the ES-compat port; the hint for the other two listeners
    /// must say how it moves them and name the key that moves only them.
    #[test]
    fn each_listener_names_its_own_key_and_how_port_moves_it() {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let os = Error::from(ErrorKind::AddrInUse);
        for (name, key) in [
            ("ES-compat", "es_compat_port"),
            ("native REST", "rest_port"),
            ("gRPC", "grpc_port"),
        ] {
            let msg = bind_error_message(name, addr, &os);
            assert!(msg.starts_with(&format!("{name}: bind {addr}: ")), "{msg}");
            assert!(msg.contains(&format!("`{key}`")), "{msg}");
            assert!(msg.contains("native REST BASE+1, gRPC BASE+2"), "{msg}");
            assert!(msg.contains("--auto-port"), "{msg}");
        }
    }

    /// #1723 / #1738: Windows' `os error 10013` is `PermissionDenied`, and it
    /// printed no hint at all. `--auto-port` is not offered: it only steps
    /// past ports that are in use.
    #[test]
    fn a_refused_port_gets_a_hint_without_auto_port() {
        let os = Error::from(ErrorKind::PermissionDenied);
        let high: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let msg = bind_error_message("native REST", high, &os);
        assert!(
            msg.starts_with("native REST: bind 127.0.0.1:8080: "),
            "{msg}"
        );
        assert!(msg.contains("--port <BASE>"), "{msg}");
        assert!(msg.contains("`rest_port`"), "{msg}");
        assert!(!msg.contains("--auto-port"), "{msg}");
        if cfg!(windows) {
            assert!(msg.contains("excludedportrange"), "{msg}");
        } else {
            let low: SocketAddr = "127.0.0.1:80".parse().unwrap();
            let msg = bind_error_message("native REST", low, &os);
            assert!(msg.contains("below 1024"), "{msg}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn wsaeacces_is_classified_as_a_refused_port() {
        // WSAEACCES, the code in both field reports.
        let os = Error::from_raw_os_error(10013);
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let msg = bind_error_message("native REST", addr, &os);
        assert!(msg.contains("excludedportrange"), "{msg}");
        assert!(msg.contains("os error 10013"), "{msg}");
    }

    /// Anything else keeps the plain `{name}: bind {addr}: {reason}`.
    #[test]
    fn other_bind_errors_are_not_given_a_port_hint() {
        let os = Error::from(ErrorKind::AddrNotAvailable);
        let addr: SocketAddr = "10.255.255.1:9200".parse().unwrap();
        let msg = bind_error_message("ES-compat", addr, &os);
        assert_eq!(msg, format!("ES-compat: bind {addr}: {os}"));
    }
}

/// If `data_dir` is currently locked by another live process, find the first
/// sibling directory (`<dir>-2`, `<dir>-3`, ...) that is not, so multiple
/// opted-in (`--auto-port`) instances can run side by side.
///
/// Advisory only: the engine re-acquires `node.lock` itself at open and fails
/// cleanly if a racing instance took the directory in between. After
/// [`MAX_DATA_DIR_SIBLINGS`] held siblings it gives up and returns the
/// requested dir, so that engine lock error is what the operator sees.
fn resolve_available_data_dir(requested: &str) -> String {
    // A trailing separator would turn `./data/` into `./data/-2` — a
    // directory INSIDE the held data dir. Siblings are built from the
    // trimmed spelling (`/` alone is left as is).
    let base = requested.trim_end_matches(['/', std::path::MAIN_SEPARATOR]);
    let base = if base.is_empty() { requested } else { base };
    if !data_dir_held(Path::new(base)) {
        return requested.to_string();
    }
    for i in 2..=MAX_DATA_DIR_SIBLINGS + 1 {
        let candidate = format!("{base}-{i}");
        if !data_dir_held(Path::new(&candidate)) {
            warn!(
                "data dir '{requested}' is held by another running xerj process; \
                 --auto-port: using '{candidate}' for this instance"
            );
            return candidate;
        }
    }
    requested.to_string()
}

/// How many `<dir>-N` siblings [`resolve_available_data_dir`] will try.
const MAX_DATA_DIR_SIBLINGS: usize = 64;

/// Is `dir/node.lock` currently locked by a live process?
///
/// A missing file, a free OS lock, or a busy lock whose recorded PID is not
/// running is not held. The engine reclaims the stale case itself; treating
/// it as held here would make `--auto-port` start on an empty sibling and
/// leave the real data directory behind. A PID that cannot be read still
/// counts as held.
fn data_dir_held(dir: &Path) -> bool {
    xerj_engine::data_dir_lock_held(dir)
}

/// Bind one listener, attempting consecutive ports if `auto_port` is true and
/// the starting port is already in use (`AddrInUse`).
fn bind_listener_candidate(
    ip: std::net::IpAddr,
    port: u16,
    name: &'static str,
    auto_port: bool,
    avoid_ports: &[u16],
) -> Result<(std::net::TcpListener, u16)> {
    // Port 0 asks the kernel for an ephemeral port, which can never be "in
    // use": there is nothing to scan, and the port to report is the one the
    // kernel handed back, not the 0 that was requested.
    if !auto_port || port == 0 {
        let addr = SocketAddr::new(ip, port);
        let listener = bind_listener(addr, name)?;
        let bound = listener
            .local_addr()
            .with_context(|| format!("{name}: read the bound address"))?
            .port();
        return Ok((listener, bound));
    }

    // Scan in u16 space without ever incrementing past 65535 (a `+= 1` there
    // would panic in debug and wrap to the ephemeral port 0 in release).
    let max_port = port.saturating_add(AUTO_PORT_SCAN);
    for candidate in port..=max_port {
        if avoid_ports.contains(&candidate) {
            continue;
        }
        let addr = SocketAddr::new(ip, candidate);
        match std::net::TcpListener::bind(addr) {
            Ok(listener) => {
                listener
                    .set_nonblocking(true)
                    .with_context(|| format!("{name}: set the listener on {addr} non-blocking"))?;
                if candidate != port {
                    warn!("{name}: port {port} is in use; --auto-port bound {candidate} instead");
                }
                return Ok((listener, candidate));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {}
            Err(e) => return Err(anyhow::anyhow!("{}", bind_error_message(name, addr, &e))),
        }
    }

    anyhow::bail!("{name}: could not find a free port between {port} and {max_port}");
}

/// How far past the requested port `--auto-port` scans.
const AUTO_PORT_SCAN: u16 = 1000;

async fn serve_on(
    router: Router,
    listener: std::net::TcpListener,
    name: &'static str,
    tls: Option<RustlsConfig>,
    release: tokio::sync::watch::Receiver<ShutdownPhase>,
) -> Result<()> {
    let addr = listener
        .local_addr()
        .with_context(|| format!("{name}: read the bound address"))?;
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
        // graceful-shutdown future, so bridge the post-flush release to it:
        // stop accepting and drain in-flight connections (10 s grace) only
        // after the final flush has finished writing the data directory.
        let handle = axum_server::Handle::new();
        let shutdown_handle = handle.clone();
        tokio::spawn(async move {
            // The port stays bound through the final flush. Closing it on the
            // signal itself is what let a concurrent wipe remove segments the
            // flush was still writing.
            wait_for_phase(release, |phase| phase == ShutdownPhase::Released).await;
            shutdown_handle.graceful_shutdown(Some(std::time::Duration::from_secs(10)));
        });

        info!("{name} listening on {addr} (TLS)");

        axum_server::from_tcp_rustls(listener, tls)
            .handle(handle)
            .serve(router.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .with_context(|| format!("{name} serve error (TLS)"))?;

        info!("{name} shut down cleanly");
        return Ok(());
    }

    // ── Plain path: unchanged cleartext HTTP ─────────────────────────────
    let listener = TcpListener::from_std(listener)
        .with_context(|| format!("{name}: adopt the bound listener on {addr}"))?;

    info!("{name} listening on {addr}");

    // TCP_NODELAY: disable Nagle on accepted connections.  Elasticsearch
    // (Netty) sets this by default; without it small request/response
    // round-trips can stall on the delayed-ACK/Nagle interaction, which
    // shows up as a fixed per-request latency tax on trivial reads.
    //
    // Bounded drain (#1635): `with_graceful_shutdown` stops accepting once
    // the shutdown flush has released the listener, then waits forever for
    // in-flight connections to drain.  A request parked on a wedged flush
    // never drains, so shutdown would hang the process until SIGKILL.
    // Mirror the TLS path's 10 s grace: once the flush releases the port,
    // give drainers 10 s, then force the listener down.
    let (drain_tx, drain_rx) = tokio::sync::oneshot::channel::<()>();
    let serve = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .tcp_nodelay(true)
    .with_graceful_shutdown(async move {
        wait_for_phase(release, |phase| phase == ShutdownPhase::Released).await;
        let _ = drain_tx.send(());
    });
    // `with_graceful_shutdown` yields an `IntoFuture`, not a `Future`; the
    // select arm polls `&mut serve`, so turn it into a concrete future first.
    let serve = std::future::IntoFuture::into_future(serve);
    tokio::pin!(serve);
    tokio::select! {
        r = &mut serve => r.with_context(|| format!("{name} serve error"))?,
        _ = async {
            let _ = drain_rx.await;
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        } => {
            warn!("{name}: in-flight connections did not drain within 10s of the shutdown flush; forcing shutdown");
        }
    }

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

/// `xerj index`'s string flag value; exits 2 on a missing one (see [`flag_value`]).
fn index_flag_value(flag: &str, next: Option<String>) -> String {
    flag_value(flag, next).unwrap_or_else(|e| {
        eprintln!("error: {e}. Use --help for usage.");
        std::process::exit(2);
    })
}

/// `xerj index`'s numeric flag value. `--batch abc` used to fall back to the
/// default without a word, so a typo silently changed the run (#1435).
fn index_flag_count(flag: &str, next: Option<String>) -> usize {
    parse_count(flag, next).unwrap_or_else(|e| {
        eprintln!("error: {e}. Use --help for usage.");
        std::process::exit(2);
    })
}

fn parse_count(flag: &str, next: Option<String>) -> Result<usize, String> {
    let v = flag_value(flag, next)?;
    v.parse()
        .map_err(|_| format!("{flag} must be a non-negative whole number, got {v:?}"))
}

/// Validate `xerj index --file` before `run_cli_index` writes any state (#1818).
///
/// Without this check a directory reached `memmap2::Mmap::map` and surfaced
/// `EINVAL` only after the data dir, lock, audit log, and target index had
/// been committed, leaving an empty phantom index behind.
///
/// `fs::metadata` follows symlinks, so a symlink to a directory is caught as
/// a directory. A missing path returns `Ok(())` so `File::open` can surface
/// its native ENOENT message. A non-regular file (FIFO, socket, device) is
/// refused here because `File::open` on a FIFO blocks until a writer appears.
fn check_index_file_input(path: &Path) -> Result<(), String> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(());
    };
    if meta.is_dir() {
        return Err(format!(
            "--file {0} is a directory, not an NDJSON file; \
             run `xerj autoindex {0}` to index a folder",
            path.display(),
        ));
    }
    if !meta.is_file() {
        return Err(format!("--file {} is not a regular file", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod check_index_file_input_tests {
    use super::check_index_file_input;

    #[test]
    fn a_regular_ndjson_file_is_accepted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("docs.ndjson");
        std::fs::write(&file, b"{}\n").expect("write ndjson");
        assert!(check_index_file_input(&file).is_ok());
    }

    #[test]
    fn an_empty_regular_file_is_accepted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("empty.ndjson");
        std::fs::write(&file, b"").expect("write empty file");
        assert!(check_index_file_input(&file).is_ok());
    }

    #[test]
    fn a_directory_is_refused_and_points_at_autoindex() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = check_index_file_input(dir.path()).expect_err("a dir is not NDJSON");
        assert!(err.contains("is a directory"), "{err}");
        assert!(err.contains("not an NDJSON file"), "{err}");
        assert!(err.contains("xerj autoindex"), "{err}");
        assert!(err.contains(&dir.path().display().to_string()), "{err}");
    }

    #[test]
    fn a_missing_path_is_passed_through_to_file_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("does-not-exist.ndjson");
        assert!(check_index_file_input(&missing).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_a_directory_is_refused_as_a_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("real-dir");
        std::fs::create_dir(&real).expect("mkdir");
        let link = dir.path().join("link-to-dir");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        let err = check_index_file_input(&link).expect_err("symlink to dir is not NDJSON");
        assert!(err.contains("is a directory"), "{err}");
        assert!(err.contains("xerj autoindex"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_is_refused_as_not_a_regular_file() {
        use std::ffi::CString;
        let dir = tempfile::tempdir().expect("tempdir");
        let fifo = dir.path().join("pipe");
        let c = CString::new(fifo.as_os_str().as_encoded_bytes()).expect("c-string");
        // SAFETY: `libc::mkfifo` is a POSIX syscall with no Rust-side invariants;
        // failure is reported via the return value, which is checked below.
        let rc = unsafe { libc::mkfifo(c.as_ptr(), 0o644) };
        if rc != 0 {
            // Some sandboxed filesystems disallow mkfifo; skip rather than fake a pass.
            eprintln!(
                "skipping fifo test: mkfifo failed ({})",
                std::io::Error::last_os_error()
            );
            return;
        }
        let err = check_index_file_input(&fifo).expect_err("fifo is not NDJSON");
        assert!(err.contains("is not a regular file"), "{err}");
        assert!(err.contains(&fifo.display().to_string()), "{err}");
    }
}

fn parse_index_args() -> IndexCmdArgs {
    // Arguments after "index", including a flag that was written before it.
    parse_index_args_from(xerj_common::argv::args_after_subcommand().into_iter()).unwrap_or_else(
        |e| {
            eprintln!("error: {e}. Use --help for usage.");
            std::process::exit(2);
        },
    )
}

fn parse_index_args_from<I>(args: I) -> Result<IndexCmdArgs, String>
where
    I: Iterator<Item = String>,
{
    let mut args = args;
    let mut index = None;
    let mut file = None;
    let mut batch = 5000usize;
    let mut workers = 0usize; // 0 = num_cpus
    let mut limit = 0usize; // 0 = all lines
    let mut config = None;
    let mut data_dir = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--index" | "-i" => index = Some(index_flag_value(&arg, args.next())),
            "--file" | "-f" => file = Some(PathBuf::from(index_flag_value(&arg, args.next()))),
            "--batch" | "-b" => batch = index_flag_count(&arg, args.next()),
            "--workers" | "-w" => workers = index_flag_count(&arg, args.next()),
            "--limit" | "-l" => limit = index_flag_count(&arg, args.next()),
            "--config" | "-c" => config = Some(PathBuf::from(index_flag_value(&arg, args.next()))),
            "--data-dir" | "-d" => data_dir = Some(index_flag_value(&arg, args.next())),
            xerj_common::feedback::DISABLE_FLAG => {}
            "--help" | "-h" => {
                print_index_help();
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    let index = index.ok_or("--index <name> is required")?;
    let file = file.ok_or("--file <path> is required")?;

    Ok(IndexCmdArgs {
        index,
        file,
        batch,
        workers,
        limit,
        config,
        data_dir,
    })
}

fn print_index_help() {
    println!("{}", index_help_text(xerj_common::feedback::enabled()));
}

fn index_help_text(feedback: bool) -> String {
    format!(
        "xerj index — ingest an NDJSON file directly into the engine\n\
         \n\
         {}\
         USAGE:\n\
         \x20\x20\x20\x20xerj index --index <name> --file <ndjson> [OPTIONS]\n\
         \n\
         OPTIONS:\n\
         \x20\x20\x20\x20--index,    -i <NAME>  Target index name (required)\n\
         \x20\x20\x20\x20--file,     -f <PATH>  Path to NDJSON file (required)\n\
         \x20\x20\x20\x20--batch,    -b <N>     Docs per batch (default 5000)\n\
         \x20\x20\x20\x20--workers,  -w <N>     Parallel ingest workers (default = num_cpus)\n\
         \x20\x20\x20\x20--limit,    -l <N>     Ingest only the first N lines (default: all)\n\
         \x20\x20\x20\x20--config,   -c <PATH>  Path to TOML config file\n\
         \x20\x20\x20\x20--data-dir, -d <PATH>  Override data directory\n\
         \x20\x20\x20\x20--disable-feedback     Do not print the feedback invitation above\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20(env XERJ_DISABLE_FEEDBACK=true)\n\
         \x20\x20\x20\x20--help,     -h         Show this help\n\
         \n\
         Bypasses HTTP/axum entirely. Bytes are pushed straight into\n\
         index_batch_turbo_raw via rayon workers. Use this for maximum\n\
         single-node ingest throughput — it's the xerj equivalent of\n\
         Lucene's IndexWriter fed from a file.",
        xerj_common::feedback::block(feedback),
    )
}

/// `xerj index` on a neural node: say how long embedding this file might take,
/// from the startup probe, before the ingest loop starts.
async fn warn_neural_cli_index(engine: &Engine, index: &xerj_engine::Index, file_bytes: u64) {
    let Ok(identity) = engine.embedding_execution_identity() else {
        return;
    };
    if identity.backend != "neural" {
        return;
    }
    let schema = index.schema().await;
    let embeds = schema.fields.iter().any(|field| field.embedding.is_some());
    if !embeds {
        eprintln!(
            "warning: embedding.mode=neural is fixed per index at ingest time. This index has no \
             semantic_text field, so {} of NDJSON is not an embedding-time estimate. Changing \
             --embed-mode does not re-embed documents already written; re-index. The faster \
             default is `xerj --embed-mode lexical` (or omit the flag).",
            xerj_autoindex::estimate::human_bytes(file_bytes)
        );
        return;
    }
    let wire = xerj_autoindex::esclient::EmbeddingExecutionIdentity {
        version: identity.version,
        backend: identity.backend,
        identity_sha256: identity.identity_sha256,
        dimensions: identity.dimensions,
        semantic_contract: identity.semantic_contract,
        resumable: identity.resumable,
        non_resumable_reason: identity.non_resumable_reason,
        ingest_probe_bytes: identity.ingest_probe_bytes,
        ingest_probe_passages: identity.ingest_probe_passages,
        ingest_probe_millis: identity.ingest_probe_millis,
    };
    match xerj_autoindex::neural_cost::note(
        &wire,
        file_bytes,
        xerj_autoindex::neural_cost::SourceBytes::NdjsonFileUpperBound,
    ) {
        Some(message) => eprintln!("warning: {message}"),
        None => eprintln!(
            "warning: embedding.mode=neural is fixed per index at ingest time. Changing \
             --embed-mode does not re-embed documents already written; re-index. The faster \
             default is `xerj --embed-mode lexical` (or omit the flag)."
        ),
    }
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

    // #1818: refuse a wrong-kind-of-path `--file` before any state is written.
    // Matches the #1586 two-line usage-error shape (exit 2, empty stdout).
    if let Err(e) = check_index_file_input(&cmd.file) {
        eprintln!("error: {e}");
        eprintln!("run `xerj index --help` for usage.");
        std::process::exit(2);
    }

    // Load config but override to a minimal in-process shape.
    let fake_cli = CliArgs {
        config: cmd.config.clone(),
        // No listener is bound for in-process indexing, so no port applies.
        port: None,
        // Never relocate the data dir here: indexing into `<dir>-2` because a
        // server holds `<dir>` would report success into a directory nobody
        // serves. The engine's node.lock error is the right answer.
        auto_port: Some(false),
        data_dir: cmd.data_dir.clone(),
        // In-process CLI indexing binds no listener at all, so the bind
        // address is irrelevant here — take whatever the config says.
        bind: None,
        insecure: true,
        embed_mode: None,
        onnx_model: None,
        onnx_tokenizer: None,
        compat_distribution: None,
        compat_version: None,
        // `xerj index` builds no AppState, so the decide ladder is not in
        // play; the fields exist only because CliArgs is shared.
        decide_mode: None,
        decide_model_dir: None,
    };
    let mut cfg = load_config(&fake_cli)?;
    // Tracing after config so the [logging] format applies (RC4-W4 item 6).
    init_tracing(&cfg.logging);
    warn_dormant_overrides(&cfg);
    warn_embed_mode_is_fixed_at_ingest(&cfg);
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
        xerj_common::resource::threads_for(xerj_common::resource::Workload::Latency)
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
    warn_neural_cli_index(&engine, &index, file_size).await;
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

#[cfg(test)]
mod fatal_reporter_tests {
    use super::{format_alloc_failure, format_fatal_signal};

    #[test]
    fn alloc_failure_line_names_size_and_align() {
        let mut buf = [0u8; 128];
        let n = format_alloc_failure(&mut buf, 4096, 8);
        let line = std::str::from_utf8(&buf[..n]).unwrap();
        assert!(line.starts_with("FATAL:"));
        assert!(line.contains("allocation"));
        assert!(line.contains("4096"));
        assert!(line.contains("align 8"));
        assert!(line.ends_with('\n'));
    }

    #[cfg(unix)]
    #[test]
    fn fatal_signal_line_names_sigabrt_without_installing_a_handler() {
        let mut buf = [0u8; 128];
        let n = format_fatal_signal(&mut buf, libc::SIGABRT);
        let line = std::str::from_utf8(&buf[..n]).unwrap();
        assert!(line.starts_with("FATAL:"));
        assert!(line.contains("SIGABRT"));
        assert!(line.contains(&libc::SIGABRT.to_string()));
        assert!(line.ends_with('\n'));
    }
}

/// `{data_dir}/crash.log`, opened once the data directory exists. `-1`
/// until then. Fatal signal and allocation-failure hooks write this fd
/// with `write(2)` and never allocate.
static CRASH_LOG_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

fn append_bytes(out: &mut [u8], mut at: usize, bytes: &[u8]) -> usize {
    for &b in bytes {
        if at >= out.len() {
            break;
        }
        out[at] = b;
        at += 1;
    }
    at
}

fn append_usize(out: &mut [u8], at: usize, mut value: usize) -> usize {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    if value == 0 {
        i -= 1;
        buf[i] = b'0';
    } else {
        while value > 0 {
            i -= 1;
            buf[i] = b'0' + (value % 10) as u8;
            value /= 10;
        }
    }
    append_bytes(out, at, &buf[i..])
}

/// Allocation-failure line. Replaces the default
/// "memory allocation of N bytes failed" message, so the size and
/// alignment have to be in the line. No heap use: the alloc hook runs
/// because the heap just failed.
fn format_alloc_failure(out: &mut [u8], size: usize, align: usize) -> usize {
    let mut at = append_bytes(out, 0, b"FATAL: memory allocation of ");
    at = append_usize(out, at, size);
    at = append_bytes(out, at, b" bytes failed (align ");
    at = append_usize(out, at, align);
    append_bytes(out, at, b")\n")
}

fn format_fatal_signal(out: &mut [u8], sig: i32) -> usize {
    let name: &[u8] = match sig {
        #[cfg(unix)]
        x if x == libc::SIGABRT => b"SIGABRT",
        #[cfg(unix)]
        x if x == libc::SIGSEGV => b"SIGSEGV",
        #[cfg(unix)]
        x if x == libc::SIGBUS => b"SIGBUS",
        #[cfg(unix)]
        x if x == libc::SIGILL => b"SIGILL",
        _ => b"signal",
    };
    let mut at = append_bytes(out, 0, b"FATAL: xerj received ");
    at = append_bytes(out, at, name);
    at = append_bytes(out, at, b" (");
    at = append_usize(out, at, sig.max(0) as usize);
    at = append_bytes(out, at, b") and is aborting\n");
    at
}

#[cfg(unix)]
fn write_fd(fd: i32, bytes: &[u8]) {
    if fd < 0 || bytes.is_empty() {
        return;
    }
    let mut off = 0usize;
    while off < bytes.len() {
        let n = unsafe {
            libc::write(
                fd,
                bytes[off..].as_ptr() as *const libc::c_void,
                bytes.len() - off,
            )
        };
        if n <= 0 {
            break;
        }
        off += n as usize;
    }
}

#[cfg(unix)]
fn report_fatal(bytes: &[u8]) {
    write_fd(libc::STDERR_FILENO, bytes);
    let fd = CRASH_LOG_FD.load(std::sync::atomic::Ordering::Relaxed);
    if fd >= 0 && fd != libc::STDERR_FILENO {
        write_fd(fd, bytes);
    }
}

#[cfg(not(unix))]
fn report_fatal(bytes: &[u8]) {
    let _ = std::io::Write::write_all(&mut std::io::stderr(), bytes);
}

#[cfg(not(target_env = "msvc"))]
fn note_alloc_failure(size: usize, align: usize) {
    let mut buf = [0u8; 128];
    let n = format_alloc_failure(&mut buf, size, align);
    report_fatal(&buf[..n]);
}

/// The disposition each fatal signal had before [`install_fatal_reporter`]
/// replaced it. Written once at startup, before the handler can run, and
/// only read inside the handler.
#[cfg(unix)]
static mut PREVIOUS_FATAL_ACTIONS: [Option<libc::sigaction>; 4] = [None; 4];

#[cfg(unix)]
const FATAL_SIGNALS: [libc::c_int; 4] = [libc::SIGABRT, libc::SIGSEGV, libc::SIGBUS, libc::SIGILL];

/// Async-signal-safe. Writes a line, puts back the disposition the signal
/// had before (the Rust runtime's guard-page handler for SIGSEGV/SIGBUS,
/// which prints "has overflowed its stack"; the default elsewhere), and
/// returns. A fault re-executes and reaches that handler with its real
/// `siginfo`; `abort()` re-raises SIGABRT after a handler returns; so the
/// process still dies with the original signal. SIGKILL cannot be caught.
/// SIGTERM is left to the server's shutdown path.
#[cfg(unix)]
extern "C" fn fatal_signal(sig: libc::c_int) {
    let mut buf = [0u8; 128];
    let n = format_fatal_signal(&mut buf, sig);
    report_fatal(&buf[..n]);
    unsafe {
        let previous = std::ptr::addr_of!(PREVIOUS_FATAL_ACTIONS).read();
        let slot = FATAL_SIGNALS.iter().position(|s| *s == sig);
        match slot.and_then(|i| previous[i]) {
            Some(action) => {
                libc::sigaction(sig, &action, std::ptr::null_mut());
            }
            None => {
                libc::signal(sig, libc::SIG_DFL);
            }
        }
        if sig != libc::SIGSEGV && sig != libc::SIGBUS && sig != libc::SIGILL {
            // A raised signal (not a fault) does not re-execute.
            libc::raise(sig);
        }
    }
}

fn install_fatal_reporter() {
    // Chain to the hook that was installed before: it prints the thread
    // name, the location and (with RUST_BACKTRACE) the backtrace. A panic
    // can be caught and the node keep serving, so the crash.log line says
    // "panicked", not FATAL; a panic that ends the process is followed by
    // the abort or exit that does.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        #[cfg(unix)]
        {
            let fd = CRASH_LOG_FD.load(std::sync::atomic::Ordering::Relaxed);
            if fd >= 0 {
                let thread = std::thread::current();
                let line = format!(
                    "xerj panicked on thread {}: {info}\n",
                    thread.name().unwrap_or("<unnamed>")
                );
                write_fd(fd, line.as_bytes());
            }
        }
        previous(info);
    }));
    #[cfg(unix)]
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = fatal_signal as *const () as libc::sighandler_t;
        libc::sigemptyset(&mut action.sa_mask);
        // SA_ONSTACK: a stack overflow faults with no stack left, so the
        // handler must run on the alternate stack the Rust runtime sets up
        // per thread.
        action.sa_flags = libc::SA_ONSTACK;
        let mut previous: [Option<libc::sigaction>; 4] = [None; 4];
        for (i, sig) in FATAL_SIGNALS.iter().enumerate() {
            let mut old: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(*sig, std::ptr::null(), &mut old) == 0 {
                previous[i] = Some(old);
            }
        }
        std::ptr::addr_of_mut!(PREVIOUS_FATAL_ACTIONS).write(previous);
        for sig in FATAL_SIGNALS {
            libc::sigaction(sig, &action, std::ptr::null_mut());
        }
    }
}

#[cfg(unix)]
fn open_crash_log(data_dir: &Path) {
    let path = data_dir.join("crash.log");
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else {
        return;
    };
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
            0o644,
        )
    };
    if fd >= 0 {
        CRASH_LOG_FD.store(fd, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(not(unix))]
fn open_crash_log(_data_dir: &Path) {}

fn main() -> Result<()> {
    // Before anything can write to stdout, including the `--help`/`--version`
    // fast paths inside `parse_args`. A node that dies must say so: the
    // allocation hook, the panic hook, and SIGABRT/SIGSEGV/SIGBUS/SIGILL
    // each write a FATAL line. SIGKILL still cannot be reported.
    install_fatal_reporter();
    #[cfg(unix)]
    restore_default_sigpipe();

    #[cfg(unix)]
    raise_nofile_limit();

    // Dispatch before creating Tokio's pool so each PDF worker stays small.
    // Before the flag hoist below, too: the hoist reads `std::env::args()`,
    // which panics on a non-UTF-8 argument, and a worker's PDF path may be
    // one. Only `args_os` is read here.
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("__extract-pdf")) {
        std::process::exit(xerj_autoindex::extract::pdf::run_worker_cli());
    }

    // `xerj --url URL corpus index name` used to die here as an unknown
    // server flag. Move those flags behind the subcommand before any
    // dispatch reads argv.
    xerj_common::argv::install_hoist(xerj_common::argv::reorder_flags_before_command(
        &std::env::args().skip(1).collect::<Vec<_>>(),
        is_top_level_command,
    ));

    // The isolated legacy-`.xls` BIFF reader. calamine 0.26.1 panics on crafted
    // records (short `Lbl`/`ExternSheet`) and the release profile is
    // `panic = abort`, so the read runs here, in a short-lived rlimit-capped
    // child, where a crash dies with the child instead of aborting the indexer.
    // Dispatched before the Tokio pool for the same smallness reason as the PDF
    // worker above.
    if matches!(
        xerj_common::argv::subcommand().as_deref(),
        Some(xerj_autoindex::extract::xls::WORKER_ARG)
    ) {
        std::process::exit(xerj_autoindex::extract::xls::run_worker_cli());
    }

    // `xerj mcp` — MCP stdio server (newline-delimited JSON-RPC on
    // stdin/stdout), proxying to an already-running XERJ node.
    //
    // Dispatched HERE, ahead of the production runtime, for two reasons:
    //
    //  1. Size. A stdio proxy that awaits one line at a time does not need
    //     8×cores worker threads at 4 MiB of stack each; two threads is the
    //     whole shape of the work. Same reasoning as the PDF worker above.
    //  2. stdout purity. MCP's stdio transport reserves stdout exclusively for
    //     the JSON-RPC stream — one stray banner line and the host's parser
    //     desynchronises. Everything below this point (`parse_args`'s
    //     --help/--version fast paths, the startup banner, the console setup
    //     link) writes to stdout; nothing above it does. `xerj-mcp` logs to
    //     stderr only, by construction.
    //
    // This is what makes the MCP server obtainable: before it, `xerj-mcp` was
    // a workspace crate that CI built and no release shipped, so the only way
    // to get it was to clone the repo and compile. Now `curl … | sh` is enough.
    if matches!(xerj_common::argv::subcommand().as_deref(), Some("mcp")) {
        let args: Vec<String> = xerj_common::argv::args_after_subcommand();
        let rt = build_runtime(2)?;
        return rt.block_on(xerj_mcp::run(&args));
    }
    if matches!(
        xerj_common::argv::subcommand().as_deref(),
        Some("batch-search")
    ) {
        let args = xerj_common::argv::args_after_subcommand();
        let rt = build_runtime(2)?;
        std::process::exit(rt.block_on(xerj_mcp::run_batch_cli(&args)));
    }
    // Cores from the resource policy, so `XERJ_NUM_CPUS` and a container CPU
    // quota reach the async runtime the same way they reach every pool (#240).
    let cores = xerj_common::resource::cores();
    // ~8× cores, clamped to a sane band; env override wins.
    let worker_threads = std::env::var("TOKIO_WORKER_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(|| (cores * 8).clamp(64, 512));
    let rt = build_runtime(worker_threads)?;
    run_to_exit(rt, async_main())
}

/// How long exit waits for blocking jobs still running after `async_main`
/// returns ("Goodbye" has been logged and the final flush is done).
const RUNTIME_SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Drive the server future to completion, then tear the runtime down without
/// waiting more than [`RUNTIME_SHUTDOWN_TIMEOUT`] for blocking jobs.
///
/// Dropping a `Runtime` joins its blocking pool with no time limit. After
/// SIGTERM the one job left there is usually a background merge, detached by
/// `spawn_blocking` and parked in the merge pool, which can spend minutes
/// compressing a large column at zstd 19 on a low-priority thread. The
/// process then outlived "Goodbye" by however long that merge had left
/// (90 s+ measured on macOS), and a supervisor's stop timeout killed it.
///
/// Walking away from that merge is safe: a merge publishes atomically through
/// `apply_merge`, and an unfinished batch leaves its input segments untouched
/// (the LOSS FIREWALL notes in `xerj-engine`'s `index.rs`). The final
/// `flush_all_force` has completed before `async_main` returns, and leaving a
/// job unfinished here is no different from the SIGKILL or power loss the
/// engine is already built to recover from.
fn run_to_exit<F: std::future::Future>(rt: tokio::runtime::Runtime, fut: F) -> F::Output {
    let out = rt.block_on(fut);
    rt.shutdown_timeout(RUNTIME_SHUTDOWN_TIMEOUT);
    out
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

/// #1840: a neural startup whose Hub cannot be reached fails once, here,
/// before any listener exists. This lives in `xerj-server` because that
/// crate's default features include `neural`, which is what
/// `cargo test --workspace` actually runs. `xerj-ai`'s own tests do not
/// enable the feature unless asked.
#[cfg(all(test, feature = "neural"))]
mod neural_startup_tests {
    use xerj_ai::neural::{startup_failure_message, NeuralConfig};
    use xerj_ai::Embedder;

    /// Closed port on loopback. Connection refused is local and immediate;
    /// nothing is sent off the machine.
    const UNREACHABLE_HUB: &str = "http://127.0.0.1:1";

    #[test]
    fn unreachable_hub_refuses_once_and_does_not_install_lexical() {
        let cache = tempfile::tempdir().unwrap();
        let cfg = NeuralConfig {
            model_id: "sentence-transformers/all-MiniLM-L6-v2".into(),
            cache_dir: Some(cache.path().to_path_buf()),
            local_dir: None,
            hub_endpoint: Some(UNREACHABLE_HUB.into()),
        };
        // `NeuralEmbedder` is not `Debug`, so `expect_err` cannot name the Ok side.
        let error = match Embedder::preload_neural(&cfg) {
            Ok(_) => panic!("a closed hub must not load a model"),
            Err(error) => error,
        };
        let message = startup_failure_message(&cfg.model_id, &error);
        assert!(
            message.contains("127.0.0.1") || message.contains("Connection refused"),
            "the cause must be the closed local hub, not a real download: {message}"
        );
        assert!(
            message.contains("xerj --embed-mode lexical"),
            "the error must give the lexical command: {message}"
        );
        assert!(
            message.contains("embedding.local_model_dir"),
            "the error must name the offline model directory: {message}"
        );
        assert!(
            message.contains("HF_ENDPOINT"),
            "the error must name the mirror variable: {message}"
        );
        assert!(
            message.contains("Refusing to start"),
            "the error must say the process will not start: {message}"
        );
        assert!(
            !message
                .to_ascii_lowercase()
                .contains("falling back to lexical"),
            "must not describe a lexical swap: {message}"
        );
        // The shared cell stays empty, so a later embed cannot pretend the
        // model loaded. Preload's return type is the neural model itself:
        // failure cannot hand back a lexical embedder.
        assert!(
            Embedder::preload_neural(&cfg).is_err(),
            "a second attempt must still fail rather than reuse a lexical stand-in"
        );
    }

    #[test]
    fn missing_local_model_refuses_with_the_same_startup_error() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = xerj_common::config::EmbeddingConfig {
            mode: "neural".into(),
            local_model_dir: dir.path().to_string_lossy().into_owned(),
            ..Default::default()
        };
        let error = xerj_engine::index::ensure_neural_ready(&cfg)
            .expect_err("an empty local model directory must refuse startup");
        let message = error.to_string();
        assert!(
            message.contains("embedding.mode=neural"),
            "must name the setting: {message}"
        );
        assert!(
            message.contains("xerj --embed-mode lexical"),
            "must give the lexical command: {message}"
        );
        assert!(
            message.contains("model.safetensors") || message.contains("missing"),
            "must name the missing local asset: {message}"
        );
        assert!(
            !message.contains("127.0.0.1"),
            "a local directory must not contact the hub: {message}"
        );
    }
}

#[cfg(test)]
mod help_text_tests {
    use super::{auth_disabled_line, embed_mode_fixed_at_ingest_warning, help_text};

    /// #1840: `--embed-mode neural` must say the model loads at startup and
    /// that a failed download refuses to start, with the lexical command.
    #[test]
    fn neural_help_says_a_failed_download_refuses_to_start() {
        let help = help_text(true);
        let neural = help
            .lines()
            .position(|line| line.contains("neural   built-in BERT"))
            .expect("neural mode must be documented");
        let window: String = help
            .lines()
            .skip(neural)
            .take(12)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            window.contains("loads at startup"),
            "neural help must say the model loads at startup:\n{window}"
        );
        assert!(
            window.contains("refuses to start"),
            "neural help must say a failed load refuses to start:\n{window}"
        );
        assert!(
            window.contains("xerj --embed-mode lexical"),
            "neural help must give the lexical command:\n{window}"
        );
        assert!(
            window.contains("embedding.local_model_dir"),
            "neural help must name the offline model directory:\n{window}"
        );
        assert!(
            window.contains("HF_ENDPOINT"),
            "neural help must name the Hub mirror variable:\n{window}"
        );
    }

    /// #1612: the flag is an ingest-time property. Help must say so next to
    /// the mode list, including the re-index requirement.
    #[test]
    fn embed_mode_help_says_the_mode_is_fixed_at_ingest() {
        let help = help_text(true);
        assert!(
            help.contains("fixed per index at ingest"),
            "help must say the mode is fixed per index:\n{help}"
        );
        assert!(
            help.contains("does not re-embed"),
            "help must say changing the flag does not re-embed:\n{help}"
        );
        assert!(
            help.contains("re-index"),
            "help must say a change requires a re-index:\n{help}"
        );
    }

    #[test]
    fn explicit_embed_mode_warns_that_it_is_fixed_at_ingest() {
        let neural = embed_mode_fixed_at_ingest_warning("neural").unwrap();
        assert!(neural.contains("fixed per index"));
        assert!(neural.contains("re-index"));
        assert!(neural.contains("xerj --embed-mode lexical"));
        assert!(embed_mode_fixed_at_ingest_warning("auto").is_none());
        assert!(embed_mode_fixed_at_ingest_warning("  ").is_none());
        assert!(embed_mode_fixed_at_ingest_warning("lexical")
            .unwrap()
            .contains("lexical"));
    }

    /// A loopback `--insecure` boot says nothing off this host can connect,
    /// then used to say anyone on the network can write. Those two lines
    /// cannot both be true.
    #[test]
    fn insecure_loopback_does_not_say_the_network_can_write() {
        let loopback = auth_disabled_line(true);
        assert!(
            loopback.contains("this machine"),
            "loopback auth line must name this machine: {loopback}"
        );
        assert!(
            !loopback.contains("network"),
            "loopback auth line must not claim the network can write: {loopback}"
        );
        let exposed = auth_disabled_line(false);
        assert!(
            exposed.contains("anyone on the network can write"),
            "a non-loopback bind is the network case: {exposed}"
        );
    }

    /// `--help` used to show `cat ./data/admin.key`. The default data dir is
    /// `./data`, and llms.txt says that path inside a folder you autoindex
    /// indexes the node's own storage. The installer starts with `~/xerj-data`.
    /// Help outranks the website, so the example agents copy has to be that path.
    #[test]
    fn insecure_help_example_keeps_the_data_dir_outside_the_tree() {
        let help = help_text(true);
        assert!(
            help.contains("~/xerj-data/admin.key"),
            "the export example must be the outside-the-tree data dir:\n{help}"
        );
        assert!(
            help.contains("USERPROFILE"),
            "Windows must get a path too:\n{help}"
        );
        assert!(
            !help.contains("cat ./data/admin.key"),
            "the old in-tree example is what agents copy"
        );
        assert!(
            help.contains("default data directory is ./data"),
            "omitting --data-dir still writes ./data, and the help must say so"
        );
    }

    /// `--insecure` disables TLS **and** auth (see `apply_overrides`, which
    /// sets `cfg.tls.enabled = false; cfg.auth.enabled = false;`), but the
    /// help said only "Disable TLS". Auth is on by default, so that flag is
    /// precisely what makes `xerj autoindex` work against a fresh server — and
    /// a user reading only `--help` could not learn that. The help must
    /// describe both halves of what the flag does.
    #[test]
    fn insecure_help_documents_auth_not_just_tls() {
        // `true` — the shipped default. Asserting against the real default also
        // proves the invitation, which splices in above USAGE:, does not reflow
        // the --insecure paragraph this test reads. (Resolution taken from the
        // maintainer's conflict analysis on #335; an earlier version passed
        // `false`, which tested less.)
        let help = help_text(true);
        let line = help
            .lines()
            .position(|l| l.contains("--insecure"))
            .expect("--insecure must be documented");
        // The flag's own paragraph: its line plus the indented continuation
        // lines under it, stopping at the next flag.
        let para: String = help
            .lines()
            .skip(line)
            .take(1)
            .chain(
                help.lines()
                    .skip(line + 1)
                    .take_while(|l| !l.trim_start().starts_with("--")),
            )
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            para.contains("TLS"),
            "--insecure must still say it disables TLS, got:\n{para}"
        );
        assert!(
            para.contains("auth"),
            "--insecure disables authentication too and the help must say so, got:\n{para}"
        );
        assert!(
            para.contains("admin.key"),
            "the help must point at the key a non-insecure server demands, got:\n{para}"
        );
        assert!(
            para.contains("XERJ_API_KEY"),
            "the help must name the env var that carries the key, got:\n{para}"
        );
    }

    /// A trailing `\` in the help string must not eat the indent of the next
    /// line. `--insecure`'s wrap lines up under its description, not column 0.
    #[test]
    fn help_keeps_the_indent_of_a_wrapped_flag_line() {
        let help = help_text(false);
        let lines: Vec<&str> = help.lines().collect();
        let flag = lines
            .iter()
            .position(|l| l.contains("--insecure"))
            .expect("--insecure must be documented");
        let wrap = lines[flag + 1];
        assert!(
            wrap.starts_with(' '),
            "the --insecure wrap lost its indent: {wrap:?}"
        );
        assert!(
            wrap.contains("Auth is ON by default"),
            "expected the --insecure description wrap, got {wrap:?}"
        );
        let index = super::index_help_text(false);
        let index_lines: Vec<&str> = index.lines().collect();
        let fb = index_lines
            .iter()
            .position(|l| l.contains("--disable-feedback"))
            .expect("index help documents --disable-feedback");
        assert!(
            index_lines[fb + 1].starts_with(' '),
            "index help wrap lost its indent: {:?}",
            index_lines[fb + 1]
        );
    }

    /// `xerj ask` and the `xerj index` line after it used to start at column 0.
    /// A trailing `\` in the help string eats the next line's source indent,
    /// and those two lines were the ones that still relied on it.
    #[test]
    fn usage_command_lines_keep_their_indent() {
        for hub in [false, true] {
            for line in super::ask_help_lines(hub).lines().filter(|l| !l.is_empty()) {
                assert!(
                    line.starts_with("    "),
                    "ask usage line lost its indent (hub={hub}): {line:?}"
                );
            }
        }
        let help = help_text(false);
        let usage = help
            .split_once("USAGE:")
            .expect("USAGE")
            .1
            .split_once("\nOPTIONS ")
            .expect("OPTIONS")
            .0;
        for line in usage.lines().filter(|l| l.contains("xerj ")) {
            assert!(
                line.starts_with("    "),
                "a USAGE line lost its indent: {line:?}"
            );
        }
        assert!(
            usage.contains("\n    xerj index "),
            "xerj index lost the indent the other commands have:\n{usage}"
        );
        assert!(
            usage.contains("\n    xerj ask "),
            "xerj ask lost the indent the other commands have:\n{usage}"
        );
    }

    /// `xerj ask` starts a node; `xerj hub stop` is how it is stopped. The
    /// top-level usage has to name both, or an agent that only reads
    /// `xerj --help` cannot find the stop command (#2280). A build with no
    /// hub key still must not advertise a hub it cannot use.
    #[test]
    fn top_level_help_lists_hub_and_how_to_stop_the_ask_node() {
        let with_key = super::ask_help_lines(true);
        assert!(
            with_key.contains("\n    xerj hub <command>"),
            "a hub-key build must list `xerj hub <command>` in the usage block:\n{with_key}"
        );
        assert!(with_key.contains("pull, find, stop"), "{with_key}");
        assert!(with_key.contains("xerj hub --help"), "{with_key}");
        let ask_at = with_key.find("    xerj ask ").expect("ask line");
        let hub_at = with_key.find("    xerj hub <command>").expect("hub line");
        let ask_entry = &with_key[ask_at..hub_at];
        assert!(
            ask_entry.contains("xerj hub stop"),
            "the xerj ask entry must name `xerj hub stop` before the hub command:\n{ask_entry}"
        );
        let hub_line = with_key
            .lines()
            .find(|l| l.contains("xerj hub <command>"))
            .expect("hub line");
        assert_eq!(
            hub_line.find("pull, find, stop"),
            Some(40),
            "the hub description must start at column 40, with the other USAGE lines: {hub_line:?}"
        );

        let help = help_text(false);
        let usage = help
            .split_once("USAGE:")
            .expect("USAGE")
            .1
            .split_once("\nOPTIONS ")
            .expect("OPTIONS")
            .0;
        if xerj_common::hub::hub_key_configured() {
            assert!(
                usage.contains("\n    xerj hub <command>") && usage.contains("xerj hub stop"),
                "top-level help must contain both when a hub key is configured:\n{usage}"
            );
        } else {
            assert!(
                !usage.contains("xerj hub"),
                "a no-key process must not advertise xerj hub from the top-level help:\n{usage}"
            );
        }

        let without = super::ask_help_lines(false);
        assert!(
            !without.contains("xerj hub"),
            "a build with no hub key must not list xerj hub:\n{without}"
        );
    }
}

/// `--decide-mode` / `--decide-model-dir` (#1057 tier 2): the CLI grammar and
/// the `DecideSettings::resolve` contract the flags parse into. The parse is
/// exercised through `parse_args_from` (no process-argv mutation, which every
/// test in this binary would race on), and the resolve assertions are split by
/// feature so both builds stay honest: without `decide-local` the warning must
/// name the rebuild, with it the ladder must actually arm.
#[cfg(test)]
mod decide_flag_tests {
    use super::{help_text, parse_args_from};
    use xerj_api::systemone_api::DecideSettings;

    fn argv(items: &[&str]) -> std::vec::IntoIter<String> {
        items
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn decide_flags_parse_with_values() {
        let args = parse_args_from(argv(&[
            "--decide-mode",
            "local",
            "--decide-model-dir",
            "/models/xerj-decide",
        ]))
        .unwrap();
        assert_eq!(args.decide_mode.as_deref(), Some("local"));
        assert_eq!(
            args.decide_model_dir.as_deref(),
            Some("/models/xerj-decide")
        );
    }

    #[test]
    fn a_value_flag_without_its_value_is_an_error_not_unset() {
        // #1435: given last, or followed by another flag, the value is
        // missing — this used to boot on the default silently.
        for line in [
            &["--insecure", "--data-dir"][..],
            &["--port"][..],
            &["-c"][..],
            &["--data-dir", "--insecure"][..],
            &["--bind", "--port", "9300"][..],
            &["--decide-model-dir"][..],
            &["--data-dir", "-k"][..],
            &["-d", "-c", "xerj.toml"][..],
        ] {
            let err = parse_args_from(argv(line)).err();
            assert!(
                err.as_deref().is_some_and(|e| e.contains("needs a value")),
                "{line:?} must be refused, got {err:?}"
            );
        }
        // A value that merely contains or starts with a single dash is fine.
        let args = parse_args_from(argv(&["-d", "-data", "--bind", "::1"])).unwrap();
        assert_eq!(args.data_dir.as_deref(), Some("-data"));
        assert_eq!(args.bind.as_deref(), Some("::1"));
        let args = parse_args_from(argv(&["--port", "-1"])).unwrap();
        assert_eq!(args.port.as_deref(), Some("-1"), "left to the port check");
    }

    #[test]
    fn an_unknown_argument_is_a_usage_error_not_a_process_exit() {
        // Usage errors exit 2 via `parse_args`. This used to `process::exit(1)`
        // inside the parser, so the documented usage code never ran.
        let err = parse_args_from(argv(&["not-a-subcommand"])).unwrap_err();
        assert!(err.contains("unknown argument: not-a-subcommand"), "{err}");
        let err = super::parse_index_args_from(argv(&["--nope"])).unwrap_err();
        assert!(err.contains("unknown argument: --nope"), "{err}");
        let err = super::parse_index_args_from(argv(&["--index", "logs"])).unwrap_err();
        assert!(err.contains("--file"), "{err}");
    }

    #[test]
    fn index_counts_refuse_a_non_number_instead_of_defaulting() {
        use super::parse_count;
        assert_eq!(parse_count("--batch", Some("200".into())), Ok(200));
        assert!(parse_count("--batch", Some("abc".into()))
            .unwrap_err()
            .contains("whole number"));
        assert!(parse_count("--workers", Some("-1".into())).is_err());
        assert!(parse_count("--limit", None)
            .unwrap_err()
            .contains("needs a value"));
    }

    #[test]
    fn help_counts_the_mcp_tools_actually_served() {
        // The count is `tool_specs()`, the same list `tools/list` returns:
        // nineteen without a hub key, twenty-two with the three hub tools.
        let served = super::mcp_tool_count();
        if xerj_common::hub::hub_key_configured() {
            assert_eq!(served, 22, "tool_specs() with a hub key: {served}");
        } else {
            assert_eq!(
                served, 19,
                "a no-key process must not count the hub tools it does not serve"
            );
        }
        let help = help_text(false);
        assert!(
            help.contains(&format!("exposes {served} tools")),
            "help must state the served tool count {served}, got:\n{help}"
        );
        let mcp_help = xerj_mcp::help_text(false);
        assert!(
            mcp_help.contains(&format!("proxies {served} tools")),
            "xerj mcp --help must state the same count {served}, got:\n{mcp_help}"
        );
    }

    #[test]
    fn decide_flags_default_to_none() {
        let args = parse_args_from(argv(&[])).unwrap();
        assert_eq!(args.decide_mode, None);
        assert_eq!(args.decide_model_dir, None);
    }

    #[test]
    fn decide_flags_parse_alongside_the_other_server_flags() {
        // Same command line an operator actually writes: the decide flags
        // must not disturb — or be disturbed by — the neighbours.
        let args = parse_args_from(argv(&[
            "--insecure",
            "--embed-mode",
            "neural",
            "--decide-mode",
            "local",
            "--decide-model-dir",
            "./decide-model",
            "--compat-distribution",
            "opensearch",
            "-d",
            "./data",
        ]))
        .unwrap();
        assert!(args.insecure);
        assert_eq!(args.embed_mode.as_deref(), Some("neural"));
        assert_eq!(args.compat_distribution.as_deref(), Some("opensearch"));
        assert_eq!(args.data_dir.as_deref(), Some("./data"));
        assert_eq!(args.decide_mode.as_deref(), Some("local"));
        assert_eq!(args.decide_model_dir.as_deref(), Some("./decide-model"));
    }

    #[test]
    fn help_documents_both_decide_flags_and_their_env_names() {
        let help = help_text(true);
        assert!(
            help.contains("--decide-mode"),
            "--decide-mode must be documented, got:\n{help}"
        );
        assert!(
            help.contains("--decide-model-dir"),
            "--decide-model-dir must be documented, got:\n{help}"
        );
        // The ladder's choices and its default, next to the flag that offers
        // them — an operator must not need DECISIONS.md to spell a valid mode.
        assert!(
            help.contains("history | local"),
            "the help must name the decide modes and the default, got:\n{help}"
        );
        // Both env halves, so a container can arm the tier without editing
        // its command line.
        assert!(help.contains("XERJ_DECIDE_MODE"));
        assert!(help.contains("XERJ_DECIDE_MODEL_DIR"));
    }

    // ── the resolve contract the flags parse into ──────────────────────────

    #[test]
    fn history_is_the_default_and_silent() {
        // Untouched boot: no flag, no env — "" resolves exactly like the
        // named default.
        for mode in ["", "history"] {
            let (settings, warning) = DecideSettings::resolve(mode, None);
            assert_eq!(warning, None, "mode {mode:?} must not warn");
            assert!(
                !settings.local_available(),
                "mode {mode:?} must not arm tier 2"
            );
        }
    }

    #[test]
    fn mode_is_trimmed_and_case_insensitive() {
        let (settings, warning) = DecideSettings::resolve("  History  ", None);
        assert_eq!(warning, None);
        assert!(!settings.local_available());
    }

    #[test]
    fn unknown_mode_warns_and_stays_on_the_history_vote() {
        let (settings, warning) = DecideSettings::resolve("sometimes", None);
        let warning = warning.expect("an unknown mode must warn");
        assert!(
            warning.contains("not a decide mode"),
            "the warning must say what is wrong, got: {warning}"
        );
        assert!(
            warning.contains("history or local"),
            "the warning must name the valid modes, got: {warning}"
        );
        assert!(!settings.local_available());
    }

    /// A default build asked for `--decide-mode local` must boot on the
    /// history vote AND say which rebuild arms the tier — the operator asked
    /// for tier 2 and silence here reads as "armed".
    #[cfg(not(feature = "decide-local"))]
    #[test]
    fn local_without_the_feature_warns_naming_the_rebuild() {
        let dir = tempfile::tempdir().unwrap();
        let (settings, warning) =
            DecideSettings::resolve("local", Some(dir.path().to_str().unwrap()));
        let warning = warning.expect("local on a feature-less build must warn");
        assert!(
            warning.contains("decide-local"),
            "the warning must name the missing feature, got: {warning}"
        );
        assert!(
            warning.contains("xerj-server"),
            "the warning must name the crate to rebuild, got: {warning}"
        );
        assert!(!settings.local_available());
    }

    /// With the feature compiled in, the flag's promise holds end to end at
    /// the settings seam: a real directory arms tier 2 silently, a missing
    /// one arms with the warning that names the path, and no directory at
    /// all stays on the history vote with the reason.
    #[cfg(feature = "decide-local")]
    #[test]
    fn local_with_a_model_directory_arms() {
        let dir = tempfile::tempdir().unwrap();
        let (settings, warning) =
            DecideSettings::resolve("local", Some(dir.path().to_str().unwrap()));
        assert_eq!(warning, None, "a present directory must not warn");
        assert!(settings.local_available(), "tier 2 must be armed");
    }

    #[cfg(feature = "decide-local")]
    #[test]
    fn local_without_a_directory_stays_on_the_history_vote() {
        let (settings, warning) = DecideSettings::resolve("local", None);
        let warning = warning.expect("local without a model dir must warn");
        assert!(
            warning.contains("XERJ_DECIDE_MODEL_DIR"),
            "the warning must name the missing input, got: {warning}"
        );
        assert!(
            warning.contains("config.json"),
            "the warning must say what the directory must hold, got: {warning}"
        );
        assert!(!settings.local_available());
    }

    #[cfg(feature = "decide-local")]
    #[test]
    fn local_with_a_missing_directory_arms_with_a_warning() {
        // Resolve's deliberate choice: the operator explicitly asked for
        // tier 2, so it arms anyway and the first request names the path —
        // pin it so the CLI inherits a loud failure, not a silent fallback.
        let (settings, warning) =
            DecideSettings::resolve("local", Some("/nonexistent/xerj-decide"));
        let warning = warning.expect("a missing directory must warn");
        assert!(
            warning.contains("is not a directory"),
            "the warning must name the problem, got: {warning}"
        );
        assert!(
            settings.local_available(),
            "an explicit local request must arm despite the warning"
        );
    }
}

/// Return every jemalloc arena's dirty pages to the OS (`arena.4096.purge`,
/// 4096 being `MALLCTL_ARENAS_ALL`).
///
/// `arena.<i>.purge` is a void control: jemalloc rejects it with EPERM unless
/// all four of `oldp`, `oldlenp`, `newp` and `newlen` are null/0
/// (`NEITHER_READ_NOR_WRITE` in jemalloc's `ctl.c`). The earlier
/// `tikv_jemalloc_ctl::raw::write(.., 0u64)` passed a value, so every purge
/// failed and the error was discarded. On Linux `background_thread:true`
/// hid it; on macOS there is no background thread, and the drain freed
/// nothing back to the OS.
#[cfg(not(target_env = "msvc"))]
fn purge_all_jemalloc_arenas() -> Result<(), i32> {
    // SAFETY: the name is a NUL-terminated literal naming a real ctl node,
    // and a void control reads and writes nothing, so null pointers and a
    // zero length are exactly what it requires.
    let rc = unsafe {
        tikv_jemalloc_sys::mallctl(
            c"arena.4096.purge".as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(rc)
    }
}

#[cfg(all(test, not(target_env = "msvc")))]
mod allocator_purge_tests {
    use super::purge_all_jemalloc_arenas;

    /// The memory drain's purge must actually run. `arena.<i>.purge` is a
    /// void control: jemalloc answers EPERM to any call that passes a new
    /// value (`ctl.c`, `NEITHER_READ_NOR_WRITE`), so a "write 0" purge is a
    /// silent no-op and the drain returns nothing to the OS.
    #[test]
    fn arena_purge_succeeds() {
        // Dirty some pages first so the purge has work to do.
        let blocks: Vec<Vec<u8>> = (0..64).map(|i| vec![i as u8; 256 * 1024]).collect();
        drop(std::hint::black_box(blocks));
        assert_eq!(
            purge_all_jemalloc_arenas(),
            Ok(()),
            "arena.4096.purge must return 0"
        );
    }
}

#[cfg(test)]
mod runtime_tests {
    use super::{build_runtime, run_to_exit};

    /// SIGTERM → "Goodbye" → exit must not wait on a blocking job that is
    /// still running (in production: a background merge compressing a large
    /// column on a low-priority thread). Dropping a Runtime joins its blocking
    /// pool with no timeout, so the process outlived "Goodbye" by as long as
    /// the job had left.
    #[test]
    fn exit_does_not_wait_for_a_running_blocking_job() {
        let rt = build_runtime(2).expect("prod runtime builds");
        let started = std::time::Instant::now();
        run_to_exit(rt, async {
            let (tx, rx) = tokio::sync::oneshot::channel::<()>();
            // Detached, like the merge task: nothing awaits it.
            drop(tokio::task::spawn_blocking(move || {
                let _ = tx.send(());
                std::thread::sleep(std::time::Duration::from_secs(30));
            }));
            // Make sure the job is really running before the server returns.
            rx.await.expect("blocking job started");
        });
        let waited = started.elapsed();
        assert!(
            waited < std::time::Duration::from_secs(10),
            "exit waited {waited:?} for a detached blocking job"
        );
    }

    #[test]
    fn runtime_builds_and_boots() {
        // The exact builder config `main` uses must construct and run a task on
        // a 4 MiB-stack worker — this is the boot smoke test for the setting.
        let rt = build_runtime(2).expect("prod runtime builds");
        assert_eq!(rt.block_on(async { 40 + 2 }), 42);
    }

    #[test]
    fn worker_stack_actually_exceeds_the_2mib_default() {
        // The boot test above passes even if `.thread_stack_size` is reverted,
        // because a default-stack runtime still boots. This one exercises the
        // WIRING: a worker task uses a ~3 MiB stack frame, which fits under
        // RT_THREAD_STACK_SIZE (4 MiB) but overflows tokio's 2 MiB default.
        // Reverting the `.thread_stack_size` line makes this abort the process
        // with a stack overflow rather than fail an assertion — a hard failure
        // either way, which is the point: the setting is load-bearing.
        //
        // `spawn` (not `block_on`) so the frame lands on a WORKER thread, the
        // one the setting sizes; `block_on` runs on the calling test thread.
        const FRAME: usize = 3 * 1024 * 1024;
        let rt = build_runtime(2).expect("prod runtime builds");
        let sum = rt.block_on(async {
            tokio::spawn(async {
                // A big stack array forces the worker frame past 2 MiB. `black_box`
                // + a real read keep the optimiser from eliding it.
                let mut buf = [0u8; FRAME];
                for (i, b) in buf.iter_mut().enumerate() {
                    *b = (i & 0xff) as u8;
                }
                std::hint::black_box(&buf);
                buf.iter().map(|&b| b as u64).sum::<u64>()
            })
            .await
            .expect("worker task completed without overflowing its stack")
        });
        // 256 repetitions of 0..=255 across 3 MiB: each full 0..255 block sums
        // to 32640; assert it ran rather than pinning the exact total.
        assert!(sum > 0, "the stack-heavy worker task actually executed");
    }
}

async fn async_main() -> Result<()> {
    // Subcommand dispatch — check argv[1] before any other parsing.
    let argv1 = xerj_common::argv::subcommand();
    if matches!(argv1.as_deref(), Some("index")) {
        let cmd = parse_index_args();
        return run_cli_index(cmd).await;
    }
    if matches!(argv1.as_deref(), Some("memory")) {
        let code = tokio::task::spawn_blocking(xerj_autoindex::memory_cli::run_memory_cli)
            .await
            .unwrap_or(1);
        std::process::exit(code);
    }
    if matches!(argv1.as_deref(), Some("search")) {
        let code = tokio::task::spawn_blocking(xerj_autoindex::search::run_search_cli)
            .await
            .unwrap_or(1);
        std::process::exit(code);
    }
    if matches!(argv1.as_deref(), Some("def")) {
        let code = tokio::task::spawn_blocking(xerj_autoindex::def::run_def_cli)
            .await
            .unwrap_or(1);
        std::process::exit(code);
    }
    if matches!(argv1.as_deref(), Some("init")) {
        let code = tokio::task::spawn_blocking(xerj_autoindex::init::run_init_cli)
            .await
            .unwrap_or(1);
        std::process::exit(code);
    }
    if matches!(argv1.as_deref(), Some("gain")) {
        let code = tokio::task::spawn_blocking(xerj_autoindex::gain::run_gain_cli)
            .await
            .unwrap_or(1);
        std::process::exit(code);
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
    if matches!(argv1.as_deref(), Some("share")) {
        // Share one indexed corpus as a link + passcode. A thin client of the
        // node's own `/_share` API; with `--tunnel` it also supervises a
        // `cloudflared` child until Ctrl-C. Synchronous internally — it runs on
        // a blocking thread like `brain`, and borrows this runtime only for
        // the portable Ctrl-C future.
        let code = tokio::task::spawn_blocking(share::run_cli)
            .await
            .unwrap_or(1);
        std::process::exit(code);
    }
    if matches!(argv1.as_deref(), Some("feedback")) {
        // Draft the agent field report XERJ asks every agent to file, and
        // optionally open its pull request. Pure ES-compat client + local
        // git/gh; fully synchronous internally — run it off the async runtime
        // like `autoindex` and `brain`.
        let code = tokio::task::spawn_blocking(xerj_autoindex::feedback::run_cli)
            .await
            .unwrap_or(1);
        std::process::exit(code);
    }
    if matches!(argv1.as_deref(), Some("code")) {
        // Reference-coding retrieval (issue #977, previously xc.py): blocking
        // ES-compat client + local state ledger, synchronous — off the
        // runtime like `def`. Routed through the hub CLI so `--def <SYMBOL>`
        // answers from the `-defs` index; every other invocation delegates
        // unchanged to `xc::run_code_cli` (Corpus Hub v2, §3).
        let args: Vec<String> = xerj_common::argv::args_after_subcommand();
        let code = tokio::task::spawn_blocking(move || xerj_hub::cli::run_code(&args))
            .await
            .unwrap_or(1);
        std::process::exit(code);
    }
    if matches!(argv1.as_deref(), Some("corpus")) {
        // Reference-coding corpus lifecycle (issue #977, previously
        // xc-corpus.sh/xc-index.sh): git + autoindex + ES-compat client, all
        // synchronous — off the runtime like `autoindex`.
        let args: Vec<String> = xerj_common::argv::args_after_subcommand();
        let code = tokio::task::spawn_blocking(move || xerj_autoindex::xc::run_corpus_cli(&args))
            .await
            .unwrap_or(1);
        std::process::exit(code);
    }
    if matches!(argv1.as_deref(), Some("hub")) {
        // Corpus Hub v2 (discussion #1374): pull pre-indexed, signed packs and
        // build them (CI). Synchronous (git + blocking HTTP) — off the runtime
        // like `corpus`.
        let args: Vec<String> = xerj_common::argv::args_after_subcommand();
        let code = tokio::task::spawn_blocking(move || xerj_hub::cli::run_hub_cli(&args))
            .await
            .unwrap_or(1);
        std::process::exit(code);
    }
    if matches!(argv1.as_deref(), Some("ask")) {
        // Corpus Hub v2 answer flow (plan §5): route the question to a hub
        // corpus, answer from its pack (pulling it the first time). Blocking
        // HTTP + process spawn — off the runtime like `hub`.
        let args: Vec<String> = xerj_common::argv::args_after_subcommand();
        let code = tokio::task::spawn_blocking(move || xerj_hub::cli::run_ask(&args))
            .await
            .unwrap_or(1);
        std::process::exit(code);
    }
    if xerj_hub::cli::is_bare_question(argv1.as_deref()) {
        // `xerj "how does lz4 frame work"`: argv[1] is not a subcommand, does
        // not start with `-`, and contains a space — the same as `xerj ask`.
        let args: Vec<String> = std::env::args().skip(1).collect();
        let code = tokio::task::spawn_blocking(move || xerj_hub::cli::run_ask(&args))
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
    warn_dormant_overrides(&cfg);
    warn_embed_mode_is_fixed_at_ingest(&cfg);

    // 3a. `server.bind_address` must be an IP literal — and must be rejected
    //     *here*, ahead of the two exposure checks that follow.
    //
    //     Both of those keep on `Config::bind_address_is_loopback`, which
    //     fails closed on a value it cannot parse. That is the right answer
    //     for a security predicate and the wrong one for a message: without
    //     this guard, `bind_address = "localhost"` is refused by 3c with
    //     «"localhost" is not loopback … would serve plain HTTP on a
    //     network-reachable interface», two statements that are simply untrue
    //     of localhost — and the remedy that refusal prescribes,
    //     `allow_insecure_network_bind = true`, does not fix anything. It only
    //     silences 3c, so the boot then runs to step 11 and dies on the bind
    //     *after* the data directory, the `.xerj_*` system indices, the master
    //     key and a first-run `admin.key` exist and the console setup link has
    //     been printed. That is the exact late-and-worse failure shape the
    //     cluster-transport commit on this branch was written to remove; this
    //     closes the same hole on the data-listener path.
    //
    //     Rejecting first means the loopback predicate only ever runs on a
    //     value that is known to be an IP, so 3b and 3c can only ever say
    //     something true — and the one honest error arrives with the other
    //     startup refusals, before anything is written to disk.
    //
    //     Host names are deliberately not resolved. Nothing regresses: the
    //     pre-#228 code composed listeners with
    //     `format!("{bind}:{port}").parse::<SocketAddr>()`, which never
    //     resolved names either — it just failed at step 11 instead of here.
    //     Keeping it that way also keeps the exposure predicates
    //     deterministic: a name resolves to different addresses on different
    //     hosts and at different times, so "is this node about to publish
    //     itself off-loopback?" would stop having a fixed answer. meilisearch
    //     took the other branch — `DEFAULT_HTTP_ADDR = "localhost:7700"`
    //     (`crates/meilisearch/src/option.rs:97`, MIT) is a name, resolved by
    //     actix's `bind()` — and pays for it with a bind that happens at
    //     `crates/meilisearch/src/main.rs:199-201`, after the index scheduler
    //     and database are already open. Approach compared, no code taken.
    if cfg.bind_ip().is_none() {
        anyhow::bail!(bad_bind_address(&cfg));
    }

    // 3b. gRPC h2c exposure — fail closed (issue #229). `grpc.rs` builds
    //     tonic without its `tls` feature, so `server.grpc_port` speaks
    //     cleartext h2c no matter what `[tls]` says. Enabling TLS and binding
    //     off-loopback would therefore encrypt two listeners and quietly
    //     leave the third in the clear on the same public interface — and
    //     because gRPC clients keep working, the operator's only clue is a
    //     banner they read once. Refuse instead, and name the one setting
    //     that says the exposure is deliberate.
    //
    //     Immediately after tracing and the 3a bind parse, on purpose: a boot
    //     that is going to be rejected must not create the data directory,
    //     mint and print a first-run admin key, or generate a certificate —
    //     and 3a creates nothing either. It is also after `load_config`, so
    //     `--insecure` (which clears `tls.enabled`) has already been applied
    //     and cannot be overruled from a config file.
    if cfg.grpc_h2c_exposed_off_loopback() {
        anyhow::bail!(
            "tls.enabled = true but the gRPC listener on {}:{} speaks cleartext h2c — \
             TLS covers the REST and ES-compat listeners only, and server.bind_address \
             = \"{}\" is not loopback, so gRPC traffic (including API keys) would cross \
             the network unencrypted while the node reports itself as TLS-enabled. \
             Refusing to start. Fix by binding to loopback (server.bind_address = \
             \"127.0.0.1\"), or terminate TLS for the gRPC port at a proxy, sidecar or \
             service mesh and declare it: tls.allow_insecure_grpc_h2c = true.",
            cfg.server.bind_address,
            cfg.server.grpc_port,
            cfg.server.bind_address,
        );
    }

    // 3c. Cleartext network exposure — fail closed (issue #228). The TLS-off
    //     twin of 3b, and the larger of the two: with `tls.enabled = false`
    //     it is not one uncovered listener but every listener, carrying the
    //     `Authorization: ApiKey` header of every request in the clear.
    //
    //     Auth being on by default does not help. It is precisely the
    //     credential that is on the wire, and it is on the wire on every
    //     interface the host happens to have — which used to include whatever
    //     the machine picked up from DHCP, because `0.0.0.0` was the default
    //     bind. That default is now loopback, so this fires only for a bind
    //     that someone actually wrote down, and it asks them to write down
    //     the consequence too.
    //
    //     Placed with 3b, before the data directory, the first-run admin key
    //     and the certificate, so a rejected boot leaves nothing behind.
    if cfg.cleartext_exposed_off_loopback() {
        anyhow::bail!(
            "server.bind_address = \"{}\" is not loopback and tls.enabled = false, so every \
             listener ({}, {}, {}) would serve plain HTTP on a network-reachable interface — \
             the API key in every Authorization header, and every document body, would cross \
             the network in cleartext. Refusing to start. Fix by binding to loopback \
             (server.bind_address = \"127.0.0.1\", the default) and putting a proxy in front, \
             or by enabling TLS (tls.enabled = true with cert_path + key_path). If something \
             already terminates TLS in front of this node — reverse proxy, sidecar, service \
             mesh, ingress, or a container network namespace — declare it: \
             server.allow_insecure_network_bind = true (env: \
             XERJ_ALLOW_INSECURE_NETWORK_BIND=true).",
            cfg.server.bind_address,
            cfg.server.rest_port,
            cfg.server.es_compat_port,
            cfg.server.grpc_port,
        );
    }

    // 3d. Explicit mode=neural loads the model before the data directory, the
    //     admin key, or any listener (#1840). A blocked Hub used to boot a
    //     node that then failed every ingest. Explicit proxy already refuses
    //     to start rather than substitute lexical vectors (#1189); neural does
    //     the same, because those vectors are not comparable. A slim build
    //     without the neural feature keeps its documented lexical fallback
    //     (identity reports lexical) — there is no model to load.
    if cfg.embedding.mode.trim().eq_ignore_ascii_case("neural") {
        #[cfg(feature = "neural")]
        {
            let embedding = cfg.embedding.clone();
            info!(
                model = %embedding.neural_model,
                "embedding.mode=neural: loading the model before opening the data directory"
            );
            tokio::task::spawn_blocking(move || {
                xerj_engine::index::ensure_neural_ready(&embedding)
            })
            .await
            .context("neural embedder load task panicked")??;
            info!("embedding.mode=neural: model loaded");
        }
        #[cfg(not(feature = "neural"))]
        {
            // Slim binary. `make_neural_embedder` warns and uses lexical when
            // an index actually embeds. Do not refuse to start.
        }
    }

    // 4. Data directory
    std::fs::create_dir_all(&cfg.server.data_dir)
        .with_context(|| format!("create data dir {}", cfg.server.data_dir))?;
    open_crash_log(Path::new(&cfg.server.data_dir));

    // 5. Admin key (first-run)
    ensure_admin_key(&mut cfg)?;
    // Point later loopback clients at this listener's key. The file stores
    // the path, not a second copy of the secret. A failure here does not
    // stop the node: discovery still has its path candidates.
    if cfg.auth.enabled {
        let key_path = Path::new(&cfg.server.data_dir).join("admin.key");
        if key_path.is_file() {
            if let Err(error) = xerj_common::localauth::publish_admin_key_for_port(
                cfg.server.es_compat_port,
                &key_path,
            ) {
                warn!(
                    "could not publish the admin-key path for port {}: {error}",
                    cfg.server.es_compat_port
                );
            }
        }
    }

    // 6. TLS certificate — fail closed (issue #200). The operator asked for
    //    TLS; if the certificate step cannot deliver it, refuse to start
    //    rather than bind the same ports in cleartext. A downgrade is
    //    invisible to every working client — the only signal is one log
    //    line — while API keys cross a wire the operator believes is
    //    encrypted. `Config::validate` has already rejected
    //    `tls.enabled = true` with empty cert/key paths at load time, so an
    //    error here means reusing or auto-generating the certificate
    //    genuinely failed (unwritable data dir, obstructed target, …), and
    //    a config that never went through `validate` (defaults) has TLS off.
    ensure_tls_cert(&mut cfg).context(
        "tls.enabled = true but TLS could not be established; refusing to \
         start rather than serve plain HTTP — fix the certificate setup, or \
         disable TLS explicitly (tls.enabled = false, or --insecure for dev)",
    )?;

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
    let storage_available = engine.cluster_state_boot_status().is_writable();

    // This node's own cluster address, composed from the parsed bind IP.
    // Needed twice — by the transport (8b) and by the console's node identity
    // (9) — so it is derived once and the two cannot disagree. `None` only
    // when `bind_address` is not an IP literal, which 8b refuses.
    let cluster_listen_addr = cfg.socket_addr(cfg.cluster.port);

    // 8b. Cluster runner (if cluster mode is enabled)
    //
    // The boot outcome is recorded for the REST layer (#1171): a configured
    // ring whose transport cannot bind degrades to single-node with one
    // ERROR line and then nothing — health reports the degraded state
    // instead of green silence. Starts as Degraded for every enabled path
    // and is only upgraded to Running when the runner actually started.
    let mut cluster_transport_outcome = if cfg.cluster.enabled {
        xerj_api::state::ClusterTransportStatus::Degraded
    } else {
        xerj_api::state::ClusterTransportStatus::Disabled
    };
    // The runner starts only after AppState exists, so the engine applier
    // and the propose handle are installed first. Console bootstrap (below)
    // still runs against the unattached engine: system indices stay local.
    let mut pending_cluster: Option<(
        ClusterNode,
        std::time::Duration,
        tokio::sync::watch::Receiver<bool>,
    )> = None;
    let _cluster_shutdown = if cfg.cluster.enabled && storage_available {
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

        // Derive this node's listen address from the server bind address +
        // cluster port — from the *parsed* IP, never by formatting
        // `"{bind}:{port}"`. That construction is the one that made
        // `bind_address = "::1"` unbootable for the data listeners (see 11);
        // it survived here because cluster mode is off by default, and it
        // failed later and worse — `Error: parse cluster listen address` after
        // the data directory and the first-run admin key already existed.
        let listen_addr = cluster_listen_addr.with_context(|| bad_bind_address(&cfg))?;
        // The node identifies itself by that address, so an IPv6 node is
        // `[::1]:9300` — the bracketed form peer entries are parsed from
        // (`SocketAddr::parse`, just above), not the unparseable `::1:9300`.
        let node_id = listen_addr.to_string();

        // The documented `peers` convention lists the FULL membership on
        // every node — "the current node's id is inferred from the entry
        // whose address matches its own" (docs/clustering) — so this node's
        // own entry is expected to be in the map. Self is not a peer: left
        // in, the Raft node counted 4 members of a 3-node ring (majority 3 =
        // unanimity, no fault tolerance) and every node sent its own
        // RequestVote back to itself over loopback TCP, counting its own
        // vote twice — the measured result was a ring that churned through
        // a new election every few hundred ms forever after ANY node died
        // (#1168). Removed here, once, so the transport peer map and the
        // Raft peer list agree. The cluster crate also excludes self
        // defensively on both layers.
        if peers.remove(&node_id).is_some() {
            info!(
                node = %node_id,
                "cluster.peers included this node's own address — excluded from the peer set \
                 (self is not a peer)"
            );
        }

        let tick = std::time::Duration::from_millis(cfg.cluster.tick_ms);

        match TcpTransport::new(node_id.clone(), listen_addr, peers, cluster_secret).await {
            Ok(transport) => {
                let peer_ids: Vec<String> = cfg
                    .cluster
                    .peers
                    .iter()
                    .filter_map(|p| p.split_once('=').map(|(id, _)| id.to_string()))
                    .filter(|id| id != &node_id)
                    .collect();

                // The ring's metadata lives in this directory. An in-memory log
                // would boot a restarted node with no shard plan. Opening it
                // is not optional: a failure here refuses to start, rather
                // than serving single-node writes the peers still consider
                // replicated.
                let raft_dir = xerj_cluster::raft_log::raft_storage_dir(&cfg.server.data_dir);
                let node = ClusterNode::with_storage(
                    node_id.clone(),
                    peer_ids,
                    Box::new(transport),
                    &raft_dir,
                )
                .with_context(|| {
                    format!(
                        "open the on-disk raft log at {}; refusing to start cluster mode \
                         with an in-memory log",
                        raft_dir.display()
                    )
                })?;
                info!(
                    node = %node_id,
                    dir = %raft_dir.display(),
                    commit = node.raft.commit_index(),
                    entries = node.raft.log_len(),
                    snapshot = node.raft.snapshot_index(),
                    "opened on-disk raft log"
                );
                cluster_transport_outcome = xerj_api::state::ClusterTransportStatus::Running;
                pending_cluster = Some((node, tick, shutdown_rx));
            }
            Err(e) => {
                error!("Failed to start cluster TCP transport: {e:#}");
                warn!("Continuing in degraded single-node mode");
            }
        }

        Some(shutdown_tx)
    } else if cfg.cluster.enabled {
        warn!(
            "cluster transport is disabled while cluster-state storage is unavailable; restart with supported bytes before rejoining peers"
        );
        None
    } else {
        info!("Cluster mode disabled — running in single-node mode");
        None
    };

    // 8c. Bind all three listeners NOW — before the console setup link, before
    //     the banner, before anything reports readiness, and fatally.
    //
    //     This used to happen at step 10/10a below, well after the console
    //     bootstrap at (old) step 9 had already built and printed its
    //     first-launch setup link from `cfg.server.es_compat_port` — the
    //     *requested* port, not the one the kernel handed back. With an
    //     ephemeral port (`es_compat_port = 0`) that produced
    //     `http://127.0.0.1:0/_xerj-console/setup#token=…`, a link the single
    //     action a first-run operator is told to take, and unreachable. It
    //     also ran ahead of the bind, so it printed to stderr whether or not
    //     the bind that follows ever succeeds (issue #469; #466 fixed the
    //     same class for the startup banner, printed from `local_addr()`
    //     right below).
    //
    //     Moving the bind here — ahead of the console bootstrap rather than
    //     just ahead of the banner — means both the setup link and the banner
    //     can only ever be built from a port this process actually holds; see
    //     `bind_listener` for why binding eagerly and fatally is itself
    //     load-bearing (issue #465).
    //
    //     Addresses are composed from the parsed IP, never by formatting
    //     `"{bind}:{port}"` — that string is unparseable for every IPv6
    //     literal (`"::1:9200"`), which made `bind_address = "::1"` a node
    //     that could not start at all. `socket_addr` cannot be `None` here:
    //     step 3a rejected an unparseable `bind_address` before the data
    //     directory existed, which is where that failure belongs. The
    //     context is kept as a backstop so that if a future edit reorders or
    //     drops 3a, this still names the setting rather than surfacing a bare
    //     `Option` unwrap.
    let ip = cfg.bind_ip().with_context(|| bad_bind_address(&cfg))?;

    let mut avoid: Vec<u16> = Vec::new();
    if cfg.cluster.enabled {
        avoid.push(cfg.cluster.port);
    }

    let (es_listener, bound_es_port) = bind_listener_candidate(
        ip,
        cfg.server.es_compat_port,
        "ES-compat",
        cfg.server.auto_port,
        &avoid,
    )?;
    avoid.push(bound_es_port);

    let (rest_listener, bound_rest_port) = bind_listener_candidate(
        ip,
        cfg.server.rest_port,
        "native REST",
        cfg.server.auto_port,
        &avoid,
    )?;
    avoid.push(bound_rest_port);

    let (grpc_listener, bound_grpc_port) = bind_listener_candidate(
        ip,
        cfg.server.grpc_port,
        "gRPC",
        cfg.server.auto_port,
        &avoid,
    )?;

    cfg.server.es_compat_port = bound_es_port;
    cfg.server.rest_port = bound_rest_port;
    cfg.server.grpc_port = bound_grpc_port;

    let bound = BoundAddrs {
        rest: rest_listener
            .local_addr()
            .context("native REST: read the bound address")?,
        es: es_listener
            .local_addr()
            .context("ES-compat: read the bound address")?,
        grpc: grpc_listener
            .local_addr()
            .context("gRPC: read the bound address")?,
    };

    // 9. Xerj Console bootstrap.  Creates `.xerj_*` system indices on first
    //    boot, persists a 32-byte master key under data_dir/.xerj_master_key
    //    (mode 0600), and prints the first-launch magic-link banner to
    //    stderr if no active user exists yet.  Idempotent on reboot.
    //
    //    This is storage activation, not cosmetic UI setup. When the engine's
    //    cluster-state compatibility fence is blocked, boot must remain live
    //    only for diagnostics: opening and seeding Console indices can replay
    //    their WAL and flush a new segment. The Console surface is therefore
    //    absent until a supported restart instead of mounting with a made-up
    //    key or partially-initialized stores.
    let xerj_console_bind_url = format!(
        "http://{}:{}",
        // Every loopback or unspecified bind is *addressed* as `localhost`
        // (issue #935): browsers refuse an IP-literal WebAuthn origin
        // outright, so a first-launch link printed with `127.0.0.1` or
        // `[::1]` could never complete passkey enrolment even once the
        // relying party is derived from this same URL. Non-loopback
        // addresses keep their spelling, IPv6 bracketed.
        xerj_console_api::public_bind_host(&cfg.server.bind_address),
        // The bound ES-compat port (8c), not `cfg.server.es_compat_port`: with
        // an ephemeral port (`= 0`) the config value is never the port the
        // node ends up listening on (issue #469).
        bound.es.port(),
    );
    let xerj_console_node_id: String = match (cfg.cluster.enabled, cluster_listen_addr) {
        // The same string the cluster transport registered as this node's id.
        (true, Some(addr)) => addr.to_string(),
        // Unreachable: 8b refuses a cluster bind that has no socket address.
        // Report the setting as written rather than invent an identity.
        (true, None) => format!("{}:{}", cfg.server.bind_address, cfg.cluster.port),
        (false, _) => "local".to_string(),
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
    let xerj_console_state = if storage_available {
        let outcome = xerj_console_api::bootstrap::run(
            &engine,
            Path::new(&cfg.server.data_dir),
            &xerj_console_bind_url,
        )
        .await
        .context("xerj-console bootstrap")?;
        Some({
            // #935: the WebAuthn relying party follows the address and port
            // the console is actually served on — `ConsoleState::new`'s
            // hard-coded `http://localhost:9200` origin meant every node on
            // another port refused the very first passkey enrolment with
            // "origin does not match". A non-loopback IP bind has no honest
            // RP (WebAuthn ids must be domains), so the console's passkey
            // sign-in is unavailable there rather than the node refusing to
            // serve its data-plane API over a console-only limitation.
            let console_rp = match RpConfig::from_bind_url(&xerj_console_bind_url) {
                Ok(rp) => rp,
                Err(why) => {
                    warn!("Xerj Console passkey sign-in is unavailable: {why}");
                    RpConfig::default()
                }
            };
            ConsoleState::new_with_rp(
                engine.clone(),
                xerj_console_node_id,
                outcome.master_key,
                xerj_console_cluster_mode,
                console_rp,
            )
            .with_trusted_proxies(trusted_proxies)
        })
    } else {
        warn!(
            "Xerj Console bootstrap and routes are disabled while cluster-state storage is unavailable"
        );
        None
    };

    // 9b. Application state
    let mut state = AppState::new(cfg.clone(), engine, metrics);

    // 9b-i. The cluster-transport boot outcome (#1171): `AppState::new`
    //   seeds the field from the config alone because it cannot see the
    //   boot; overwrite it with what actually happened in step 8b before
    //   any router exists to read it. A node whose configured ring failed
    //   to bind (or whose cluster-state storage was unavailable) serves
    //   single-node while the operator believes it is clustered — this is
    //   what lets /_cluster/health say yellow instead of green forever.
    state.cluster_transport = cluster_transport_outcome;

    // Metadata writes propose through this handle. The runner applies
    // committed entries onto `state.engine` before any router is built, so
    // the first request already sees a node that can elect and replicate.
    if let Some((mut node, tick, shutdown_rx)) = pending_cluster {
        node.set_applier(std::sync::Arc::new(xerj_engine::EngineMetadataApplier {
            engine: std::sync::Arc::clone(&state.engine),
        }));
        let node_id = node.raft.id.clone();
        let handle = node.handle();
        // Attach before replay. Replica-store recovery asks which node this
        // is; before attach, that id is the single-node "local" and no plan
        // names it.
        state.engine.attach_cluster(handle.clone());
        // Document and search forwarding. Installed before the runner and
        // before HTTP accept, so a committed shard plan can be served.
        handle.set_shard_handler(std::sync::Arc::new(
            xerj_api::cluster_shard::ApiShardHandler::new(state.clone()),
        ));
        node.replay_committed().await;
        let mut runner = ClusterRunner::new(node, tick, shutdown_rx);
        info!(node = %node_id, "Starting cluster runner (Raft mode)");
        tokio::spawn(async move { runner.run().await });
    }

    // 9b-iia. Decide-ladder override (#1057): `--decide-mode` /
    //   `--decide-model-dir` flags or XERJ_DECIDE_MODE / XERJ_DECIDE_MODEL_DIR
    //   env (flags win; the flag can fill either half, so `--decide-mode local`
    //   alone honours an env-set XERJ_DECIDE_MODEL_DIR) — the same
    //   flag-over-env shape as `--embed-mode`. `AppState::new` already
    //   resolved the env half; when a decide flag is present we re-resolve
    //   with it through the same `DecideSettings::resolve` and replace the
    //   `state.decide` seam (pub, and replaced rather than mutated, so no
    //   other holder can observe a half-built tier). `resolve` is a warning,
    //   never a panic: an unarmable request boots on the history vote with
    //   the reason in the log, exactly as the env path does.
    if args.decide_mode.is_some() || args.decide_model_dir.is_some() {
        let mode = args
            .decide_mode
            .clone()
            .or_else(|| std::env::var("XERJ_DECIDE_MODE").ok())
            .unwrap_or_default();
        let model_dir = args
            .decide_model_dir
            .clone()
            .or_else(|| std::env::var("XERJ_DECIDE_MODEL_DIR").ok());
        let (settings, warning) =
            xerj_api::systemone_api::DecideSettings::resolve(&mode, model_dir.as_deref());
        info!(
            mode = %if mode.is_empty() { "history" } else { mode.as_str() },
            from = "CLI",
            "decide ladder resolved (tier 2 {})",
            if settings.local_available() { "armed" } else { "off" }
        );
        if let Some(warning) = warning {
            warn!("{warning}");
        }
        state.decide = std::sync::Arc::new(settings);
    }

    // 9b-i. Observability wiring (RC4 W4 item 2).
    //   (a) Install the process-wide metrics handle the engine records
    //       flush/merge/WAL/index-latency observations into — the flush, merge
    //       and WAL write sites live deep in xerj-engine, which is built before
    //       `Metrics` exists and holds no handle to it.
    // (b) used to spawn a 10 s gauge refresher here. #874 removed it: its
    //     per-tick cost walked every index's WAL subtree on an async runtime
    //     worker — O(indices x wal shards) of stat()s whether anyone read the
    //     values or not, ~0.7 % of one core at 450 idle indices (measured in
    //     benchmarks/idle-budget/README.md). The /v1/metrics handler now
    //     refreshes doc_count / segment_count / wal_size_bytes / memory_usage
    //     at scrape time, so the gauges are live exactly when they are
    //     observable and cost nothing while nobody is looking.
    xerj_engine::set_engine_metrics(state.metrics.clone());

    // 9b-2. WAL tap (issue #320) — one-directional push of an allowlisted
    //       index subset to an external ES-compatible target. The loop always
    //       runs; it is inert (one sleep per poll interval, no I/O) until
    //       `wal_tap.enabled` and a `target_url` are set, either in the config
    //       file or at runtime via `PUT /_xerj/wal_tap`.
    {
        let tap = state.engine.wal_tap.clone();
        let engine = state.engine.clone();
        if tap.config().enabled {
            let cfg = tap.config();
            info!(
                // Redacted: a `user:pass@host` URL is refused at startup, but
                // a log line is the wrong place to find out that one slipped
                // through some other path.
                target = %cfg.redacted_target_url(),
                indices = ?cfg.indices,
                poll_interval_ms = cfg.poll_interval_ms,
                "WAL tap enabled — pushing to an external ES-compatible target \
                 (one-directional, no backfill; see GET /_xerj/wal_tap/_stats)"
            );
        }
        tokio::spawn(xerj_engine::wal_tap::run(tap, engine));
    }

    // 9c. Routers — engine and Xerj Console are *peer* surfaces.  Each crate
    //     builds a complete Router (routes + its own auth + its own
    //     middleware) and `xerj-server` merges them onto the same TCP
    //     listeners.  Engine layers (admin Bearer auth) apply only to
    //     engine routes; Xerj Console's session-cookie auth applies only to
    //     /_xerj-console/api/v1/* routes.  Yanking xerj-console-api out of this
    //     merge would leave xerj-api compiling and serving on its own
    //     — a property worth preserving.
    let mut native_router = build_native_router(state.clone());
    // One hub state for the ES-compat `/_hub/v1/*` routes AND the gRPC
    // `XerjHub` service: one rate-limit budget per client across both.
    let hub_shared = xerj_api::hub_api::HubShared::new(state.clone());
    let mut es_router = build_es_compat_router_with_hub(state.clone(), hub_shared.clone());
    if let Some(console_state) = xerj_console_state {
        let xerj_console_router = xerj_console_api::xerj_console_router(console_state);
        native_router = native_router.merge(xerj_console_router.clone());
        es_router = es_router.merge(xerj_console_router);
    }
    // Outermost, so it runs before auth: once shutdown starts, new HTTP work
    // is refused while the listening socket stays open. `/health/live` is
    // exempt inside the layer.
    let drain = DrainGate::new();
    native_router = hold_port_while_draining(native_router, drain.clone());
    es_router = hold_port_while_draining(es_router, drain.clone());
    let (phase_tx, phase_rx) = tokio::sync::watch::channel(ShutdownPhase::Serving);

    // 10/10a. Bind addresses + listeners: moved up to step 8c, ahead of the
    //         console bootstrap at step 9, so the setup link it prints is
    //         built from a port this process actually holds (issue #469).
    //         `rest_addr`/`es_addr`/`grpc_addr`, the three listeners, and
    //         `bound` all still live from there.

    // 11. Banner (includes total startup time). Printed from `bound` (8c) —
    //     the addresses the kernel gave us — so every port on it is one this
    //     process holds.
    let startup_ms = startup_start.elapsed().as_millis();
    info!("startup complete in {}ms", startup_ms);
    print_banner(&cfg, &bound, startup_ms);

    // 12. Background flush timer
    let flusher = storage_available.then(|| {
        spawn_periodic_flusher(
            state.engine.clone(),
            std::time::Duration::from_secs(cfg.storage.flush_interval_secs),
        )
    });

    // 12b. Resource governor sampler (RC4 W3 items 1 & 3): refreshes the
    //      process-wide memtable / RSS / disk-usage atomics that drive the
    //      parent circuit breaker and the disk flood-stage write block. This
    //      is the structural guard against the 112 GiB OOM class — writes get
    //      a 429 circuit_breaking_exception before the kernel OOM-kills us.
    //      Since #948 the sampler also requests a memory-pressure DRAIN
    //      (flush + cache release) when the breaker engages; the purge hook
    //      installed just below is what lets that drain actually lower RSS
    //      under jemalloc, which otherwise keeps freed pages resident.
    if storage_available {
        // The purge-hook closure calls jemalloc mallctls, and jemalloc is not
        // built on MSVC (the tikv crates are target-gated out in Cargo.toml),
        // so the hook installs on every other target only — the engine's
        // default no-op purge covers MSVC. The resource sampler below still
        // runs everywhere: allocator_snapshot() has its own msvc fallback.
        #[cfg(not(target_env = "msvc"))]
        xerj_engine::engine::set_allocator_purge_hook(std::sync::Arc::new(|| {
            // Refresh the cached stats first (the epoch mallctl is what makes
            // the stats reads in the ingest-memory ledger coherent), then
            // purge every arena's dirty pages back to the OS. 4096 is
            // MALLCTL_ARENAS_ALL. Both are best-effort: a failing mallctl
            // must never propagate out of the drain path.
            let _ = tikv_jemalloc_ctl::epoch::advance();
            // Best-effort as before, but no longer silent: a purge that
            // fails is a drain that returns nothing to the OS. Warn once.
            if let Err(rc) = purge_all_jemalloc_arenas() {
                static WARNED: std::sync::Once = std::sync::Once::new();
                WARNED.call_once(|| {
                    warn!("jemalloc arena.4096.purge failed (mallctl returned {rc}); memory drains will not return pages to the OS");
                });
            }
        }));
        state.engine.spawn_resource_sampler();
    }

    // 12c. ISM/ILM lifecycle-management background job: drives every
    //      managed index's state machine (actions, then transitions) on
    //      `lifecycle.tick_interval_secs` (default 5 minutes).
    if storage_available {
        state.engine.spawn_lifecycle_manager();
    }

    let _ingest_memory_trace = storage_available.then(|| ingest_memory_trace::spawn(&state.engine));
    // 13. Start servers concurrently. Their sockets stay open until
    // `phase_tx` says the final flush has finished; see `ShutdownPhase`.
    let rest_tls = tls_config.clone();
    let rest_phase = phase_rx.clone();
    let mut rest = tokio::spawn(async move {
        if let Err(e) = serve_on(
            native_router,
            rest_listener,
            "native REST",
            rest_tls,
            rest_phase,
        )
        .await
        {
            error!("native REST: {e:#}");
        }
    });

    let es_tls = tls_config.clone();
    let es_phase = phase_rx.clone();
    let mut es = tokio::spawn(async move {
        if let Err(e) = serve_on(es_router, es_listener, "ES-compat", es_tls, es_phase).await {
            error!("ES-compat: {e:#}");
        }
    });

    // gRPC closes when draining starts, before the flush. It is not the port
    // a readiness probe uses. The bind already succeeded above — a *later*
    // transport failure is logged rather than fatal, because by then the
    // process has made no false claim: it did hold the port.
    let grpc_state = state.clone();
    let grpc_phase = phase_rx;
    let mut grpc = tokio::spawn(async move {
        if let Err(e) = grpc::serve_grpc_on(
            grpc_listener,
            grpc_state,
            hub_shared,
            wait_for_phase(grpc_phase, |phase| phase != ShutdownPhase::Serving),
        )
        .await
        {
            error!("gRPC server: {e:#}");
        }
    });

    // 14. Park until the signal, or until a listener task returns on its own
    // (a transport error). Do not join the HTTP listeners here: they are what
    // keeps the port bound through the flush below.
    tokio::select! {
        _ = shutdown_signal() => {}
        _ = &mut rest => {}
        _ = &mut es => {}
        _ = &mut grpc => {}
    }

    drain.begin();
    // gRPC stops accepting now. HTTP keeps the socket and answers 503.
    let _ = phase_tx.send(ShutdownPhase::Draining);
    let _ = grpc.await;

    let drain_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while drain.in_flight() > 0 && std::time::Instant::now() < drain_deadline {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    if drain.in_flight() > 0 {
        warn!("in-flight HTTP requests did not finish within 10s of shutdown; flushing anyway");
    }

    // Stop the periodic flusher BEFORE the final synchronous flush so its
    // next tick can't race `flush_all_force`.
    //
    // RC4 W2 #20: this abort used to sit at startup, above the `join!`,
    // killing the flusher milliseconds after it was spawned.
    // `flush_interval_secs` was inert for the entire life of the server: an
    // over-threshold memtable that the write-path flush scheduler missed
    // (WAL replay at boot, flush-permit exhaustion on the last write) sat in
    // memory — pinning its WAL generations on disk — until shutdown or the
    // next explicit `_flush`. Aborting here is after the server has been
    // serving; the signal is what reached this line.
    if let Some(flusher) = flusher {
        flusher.abort();
    }

    // 15. Final synchronous flush across every index, WHILE the HTTP ports
    // are still bound.
    //
    // The flush has to land memtables as segments before the process exits.
    // Without it, anything bulk-ingested after the last auto-flush threshold
    // lives only in the WAL until the next startup — and an index whose
    // memtable never reached that threshold loses its data on restart because
    // startup index discovery looks for segment-bearing directories first.
    // See POV report 2026-04-24T23-58-00 (B-2).
    //
    // The port stays open across this flush. Closing it first made connection
    // refusal mean "still writing", and a wipe that started at that signal
    // deleted the segment files the flush was creating.
    if storage_available {
        info!("flushing in-memory state before exit…");
        let flush_started = std::time::Instant::now();
        // Bounded (#1635): a wedged memtable flush must not re-hang shutdown.
        // Docs live in the WAL and replay on the next boot, so a skipped
        // final flush is non-destructive.
        let flush_budget = std::time::Duration::from_secs(10);
        match tokio::time::timeout(flush_budget, state.engine.flush_all_force()).await {
            Ok(()) => info!("final flush complete in {:.0?}", flush_started.elapsed()),
            Err(_) => warn!(
                "final flush exceeded {flush_budget:?}; exiting anyway (WAL replays on next boot)"
            ),
        }
    }

    let _ = phase_tx.send(ShutdownPhase::Released);
    let _ = rest.await;
    let _ = es.await;

    info!("xerj v{} stopped. Goodbye.", env!("CARGO_PKG_VERSION"));
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod shutdown_port_tests {
    use super::*;

    /// The use-case harness (and anything else that wipes a data directory
    /// when connect() fails) must still be able to connect while the shutdown
    /// flush would be running, and must fail only after the flush releases
    /// the listener.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn http_port_stays_bound_until_the_shutdown_flush_releases_it() {
        let (tx, rx) = tokio::sync::watch::channel(ShutdownPhase::Serving);
        let gate = DrainGate::new();
        let app = hold_port_while_draining(
            Router::new().route("/health/ready", axum::routing::get(|| async { "ready" })),
            gate.clone(),
        );
        let listener = bind_listener("127.0.0.1:0".parse().unwrap(), "test-shutdown").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            serve_on(app, listener, "test-shutdown", None, rx)
                .await
                .unwrap();
        });

        let client = reqwest::Client::new();
        let url = format!("http://{addr}/health/ready");
        let mut saw_ok = false;
        for _ in 0..50 {
            if let Ok(resp) = client.get(&url).send().await {
                assert_eq!(resp.status(), 200);
                saw_ok = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(saw_ok, "listener did not answer before shutdown");

        gate.begin();
        let draining = client.get(&url).send().await.unwrap();
        assert_eq!(
            draining.status(),
            503,
            "new HTTP work is refused once draining starts"
        );
        // The socket is still ours: connect succeeds. This is the window the
        // harness used to treat as "process exited".
        std::net::TcpStream::connect(addr).expect("port stays bound while draining");

        tx.send(ShutdownPhase::Released).unwrap();
        server.await.unwrap();

        let mut refused = false;
        for _ in 0..20 {
            if std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(50))
                .is_err()
            {
                refused = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            refused,
            "port must close once the shutdown flush releases it"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn liveness_stays_200_while_other_routes_refuse() {
        let gate = DrainGate::new();
        let app = hold_port_while_draining(
            Router::new()
                .route("/health/live", axum::routing::get(|| async { "live" }))
                .route("/health/ready", axum::routing::get(|| async { "ready" })),
            gate.clone(),
        );
        let listener = bind_listener("127.0.0.1:0".parse().unwrap(), "test-live").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::watch::channel(ShutdownPhase::Serving);
        let server = tokio::spawn(async move {
            serve_on(app, listener, "test-live", None, rx)
                .await
                .unwrap();
        });
        let client = reqwest::Client::new();
        let live = format!("http://{addr}/health/live");
        let ready = format!("http://{addr}/health/ready");
        for _ in 0..50 {
            if client.get(&live).send().await.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        gate.begin();
        assert_eq!(client.get(&live).send().await.unwrap().status(), 200);
        assert_eq!(client.get(&ready).send().await.unwrap().status(), 503);
        assert_eq!(
            gate.in_flight(),
            0,
            "a refused request must not be counted in flight"
        );
        tx.send(ShutdownPhase::Released).unwrap();
        server.await.unwrap();
    }
}

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

    /// Issue #200: a failure inside the certificate step must surface as an
    /// error with `tls.enabled` left ON, so the caller (`async_main`) aborts
    /// startup. Before the fix the call site swallowed the error and flipped
    /// `tls.enabled = false`, downgrading an operator-requested HTTPS
    /// listener to cleartext on the same ports.
    ///
    /// Scenario: `Config::validate` rejects `tls.enabled = true` with empty
    /// paths outright, so the reachable failure shape is non-empty cert/key
    /// paths whose files are missing (→ the auto-generation branch) with the
    /// generation target obstructed. A directory squatting on
    /// `<data_dir>/xerj.crt` makes the cert write fail deterministically on
    /// every platform, for root too.
    #[test]
    fn ensure_tls_cert_failure_errors_without_downgrading() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.server.data_dir = dir.path().to_string_lossy().into_owned();
        cfg.tls.enabled = true;
        cfg.tls.cert_path = dir
            .path()
            .join("missing.crt")
            .to_string_lossy()
            .into_owned();
        cfg.tls.key_path = dir
            .path()
            .join("missing.key")
            .to_string_lossy()
            .into_owned();
        std::fs::create_dir_all(dir.path().join("xerj.crt")).unwrap();

        let res = ensure_tls_cert(&mut cfg);
        assert!(
            res.is_err(),
            "an obstructed cert auto-generation target must error"
        );
        assert!(
            cfg.tls.enabled,
            "a TLS setup failure must never flip tls.enabled off — \
             fail closed, not fail open"
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

    /// The first-run banner used to print the 64-hex admin key on stdout.
    /// Agents that capture server output then hold the credential. The notice
    /// names the file and nothing else that is the key.
    #[test]
    fn first_run_notice_names_the_key_file_and_not_the_key() {
        let key = "ab".repeat(32);
        assert_eq!(key.len(), 64);
        let path = Path::new("/var/lib/xerj/admin.key");
        let notice = first_run_admin_key_notice(&key, path);
        assert!(
            !notice.contains(&key),
            "notice leaked the admin key:\n{notice}"
        );
        assert!(notice.contains(path.to_str().unwrap()), "{notice}");
        assert!(
            notice.contains("not printed"),
            "notice must say the key is withheld: {notice}"
        );
        assert!(
            notice.split_whitespace().all(|word| {
                let bare = word.trim_matches(|c: char| !c.is_ascii_hexdigit());
                !(bare.len() >= 64 && bare.bytes().all(|b| b.is_ascii_hexdigit()))
            }),
            "notice contains a 64-hex token:\n{notice}"
        );
    }

    /// A fresh data dir gets a 0600 admin.key, and a second call reuses it
    /// instead of minting another. The in-memory key matches the file.
    #[test]
    fn ensure_admin_key_writes_the_file_and_reuses_it() {
        let dir = TempDir::new().unwrap();
        let mut cfg = Config::default();
        cfg.auth.enabled = true;
        cfg.auth.admin_api_key.clear();
        cfg.server.data_dir = dir.path().to_str().unwrap().to_string();

        ensure_admin_key(&mut cfg).unwrap();
        let written = std::fs::read_to_string(dir.path().join("admin.key")).unwrap();
        assert_eq!(written, cfg.auth.admin_api_key);
        assert_eq!(written.len(), 64);
        assert!(written.bytes().all(|b| b.is_ascii_hexdigit()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("admin.key"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "admin.key must be 0600, got {mode:o}");
        }

        let first = cfg.auth.admin_api_key.clone();
        cfg.auth.admin_api_key.clear();
        ensure_admin_key(&mut cfg).unwrap();
        assert_eq!(
            cfg.auth.admin_api_key, first,
            "a restart must reuse admin.key"
        );
    }
}

#[cfg(test)]
mod auto_port_tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, TcpListener};

    const LO: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

    fn held_port() -> (TcpListener, u16) {
        let l = TcpListener::bind((LO, 0)).unwrap();
        let p = l.local_addr().unwrap().port();
        (l, p)
    }

    #[test]
    fn auto_port_is_off_unless_asked_for() {
        // No config file, no flag: a taken port must stay fatal (#465).
        let args = parse_args_from(["--insecure".to_string()].into_iter()).unwrap();
        if std::env::var_os("XERJ_AUTO_PORT").is_none() {
            assert!(!load_config(&args).unwrap().server.auto_port);
        }
        let args = parse_args_from(["--auto-port".to_string()].into_iter()).unwrap();
        assert!(load_config(&args).unwrap().server.auto_port);
    }

    #[test]
    fn taken_port_is_fatal_without_auto_port() {
        let (_held, p) = held_port();
        assert!(bind_listener_candidate(LO, p, "t", false, &[]).is_err());
    }

    #[test]
    fn taken_port_moves_with_auto_port() {
        let (_held, p) = held_port();
        let (l, got) = bind_listener_candidate(LO, p, "t", true, &[]).unwrap();
        assert_ne!(got, p);
        assert_eq!(l.local_addr().unwrap().port(), got);
    }

    #[test]
    fn avoided_ports_are_skipped() {
        let (held, p) = held_port();
        drop(held); // p is free now, but listed as reserved for a sibling
        let (_l, got) = bind_listener_candidate(LO, p, "t", true, &[p]).unwrap();
        assert_ne!(got, p);
    }

    #[test]
    fn ephemeral_port_reports_the_kernel_port() {
        // Port 0 + auto_port used to report 0, push 0 onto the avoid list, and
        // then scan the next listener from port 1.
        let (_a, pa) = bind_listener_candidate(LO, 0, "a", true, &[]).unwrap();
        let (_b, pb) = bind_listener_candidate(LO, 0, "b", true, &[pa]).unwrap();
        assert_ne!(pa, 0);
        assert_ne!(pb, 0);
        assert_ne!(pa, pb);
    }

    fn hold(dir: &Path) -> std::fs::File {
        std::fs::create_dir_all(dir).unwrap();
        let f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("node.lock"))
            .unwrap();
        f.try_lock().unwrap();
        f
    }

    #[test]
    fn free_data_dir_is_kept() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("data");
        let d = d.to_str().unwrap();
        assert_eq!(resolve_available_data_dir(d), d);
        // A stale lock file from a dead process is not "held".
        std::fs::create_dir_all(d).unwrap();
        std::fs::write(Path::new(d).join("node.lock"), "12345").unwrap();
        assert_eq!(resolve_available_data_dir(d), d);
    }

    #[test]
    fn a_busy_lock_recorded_against_a_dead_pid_does_not_move_when_readable() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("data");
        std::fs::create_dir_all(&d).unwrap();
        let dead = reaped_pid();
        let lock_path = d.join("node.lock");
        std::fs::write(&lock_path, format!("{dead}\n")).unwrap();
        let holder = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        holder.try_lock().unwrap();
        let readable = std::fs::read_to_string(&lock_path)
            .ok()
            .is_some_and(|text| text.trim().parse::<u32>().ok() == Some(dead));
        let ds = d.to_str().unwrap();
        if readable {
            // Advisory lock: the PID can be shown to be dead, so --auto-port
            // stays on this directory and the engine reclaims the lock.
            assert_eq!(resolve_available_data_dir(ds), ds);
        } else {
            // Mandatory lock hides the PID. A live handle still counts as held.
            assert_eq!(resolve_available_data_dir(ds), format!("{ds}-2"));
        }
        drop(holder);
    }

    fn reaped_pid() -> u32 {
        let mut cmd = if cfg!(windows) {
            let mut cmd = std::process::Command::new("cmd");
            cmd.args(["/C", "exit", "0"]);
            cmd
        } else {
            std::process::Command::new("true")
        };
        let mut child = cmd.spawn().expect("spawn a process that exits");
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    #[test]
    fn held_data_dir_moves_to_a_sibling() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("data");
        let _g1 = hold(&d);
        let _g2 = hold(&t.path().join("data-2"));
        let ds = d.to_str().unwrap();
        assert_eq!(resolve_available_data_dir(ds), format!("{ds}-3"));
        // Trailing separator: the sibling must not land INSIDE the held dir.
        assert_eq!(
            resolve_available_data_dir(&format!("{ds}/")),
            format!("{ds}-3")
        );
    }
}
