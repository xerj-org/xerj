//! Issue #485: a legitimate, authenticated request whose body arrives in small
//! HTTP/2 DATA frames must not be answered `GOAWAY ENHANCE_YOUR_CALM`.
//!
//! `h2` 0.4.16 shipped the RUSTSEC-2026-0258 mitigation as a per-connection
//! byte budget: a DATA frame under 256 bytes consumes `256 - payload_len` of
//! it, a frame of 256 or more replenishes, and the budget is a fixed 25,600
//! bytes (`DEFAULT_DATA_FRAME_OVERHEAD_THRESHOLD * 100`). The charge lands when
//! the frame arrives and the refund only when the application reads it, so a
//! client that chunks small — a streaming JSON encoder, a per-document flush, a
//! gRPC stream of small messages — drains the whole budget before the handler
//! is scheduled and has its *connection* closed. Run against the pre-fix
//! dependency, the first test below is answered `GOAWAY ENHANCE_YOUR_CALM
//! (too_many_data_frames)` part-way through a 40 KB body.
//!
//! Two things make this a process-level test rather than a unit test:
//!
//! 1. The budget is enforced inside `h2`, below hyper, below axum. Nothing in
//!    XERJ's own code can be called to observe it — only a real HTTP/2 peer
//!    sending real DATA frames at a real listener can.
//! 2. The frame size is what matters, and no ordinary HTTP client lets you
//!    choose it. These tests therefore speak HTTP/2 directly (`h2` as a client,
//!    prior-knowledge h2c against the ES-compat listener) so every `send_data`
//!    is exactly one DATA frame of exactly the size asked for.
//!
//! The pair is deliberate. The first test is the regression — the traffic that
//! must work. The second is the guard on the fix: the empty-DATA-frame flood
//! the advisory was actually filed about must still be cut off, so raising the
//! headroom for legitimate clients cannot be mistaken for switching the
//! protection off.

use std::io::Read;
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::http::Request;
use tokio::net::TcpStream;

/// Fixed key so the request below is unambiguously *authenticated*: the issue
/// is about traffic that would otherwise have succeeded, not about a 401 path.
const ADMIN_KEY: &str = "issue485-admin-key-not-a-secret";

/// 100 bytes — the size named in the issue, and well inside the band that the
/// fixed 25,600-byte budget rejects (`256 - 100 = 156` charged per frame).
const CHUNK: usize = 100;

/// TOML-safe rendering of a temp path (Windows backslashes would be escape
/// sequences inside a basic string; forward slashes work everywhere).
fn toml_path(p: &std::path::Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Three distinct free ports, held simultaneously so they cannot collide, then
/// released for the child. `Config::validate` requires all three to differ.
fn three_free_ports() -> (u16, u16, u16) {
    let held: Vec<TcpListener> = (0..3)
        .map(|_| TcpListener::bind("127.0.0.1:0").unwrap())
        .collect();
    let p: Vec<u16> = held
        .iter()
        .map(|l| l.local_addr().unwrap().port())
        .collect();
    (p[0], p[1], p[2])
}

/// Kills the child on the way out, including when a test panics — a leaked
/// server would hold its data dir and its ports for the rest of the run.
struct Server {
    child: Child,
    es_port: u16,
    _dir: tempfile::TempDir,
    output: Arc<Mutex<String>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn drain_into(mut r: impl Read + Send + 'static, buf: Arc<Mutex<String>>) {
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        while let Ok(n) = r.read(&mut chunk) {
            if n == 0 {
                break;
            }
            buf.lock()
                .unwrap()
                .push_str(&String::from_utf8_lossy(&chunk[..n]));
        }
    });
}

/// Boot the real binary with authentication ON and wait until the ES-compat
/// listener answers its readiness probe.
async fn boot() -> Server {
    let dir = tempfile::tempdir().unwrap();
    let (rest, grpc, es) = three_free_ports();
    let config = format!(
        r#"
[server]
bind_address = "127.0.0.1"
rest_port = {rest}
grpc_port = {grpc}
es_compat_port = {es}
data_dir = "{data}"

[auth]
enabled = true
admin_api_key = "{ADMIN_KEY}"

[limits]
disk_flood_stage_percent = 0
"#,
        data = toml_path(&dir.path().join("data")),
    );
    let config_path = dir.path().join("xerj.toml");
    std::fs::write(&config_path, config).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_xerj"))
        .arg("--config")
        .arg(&config_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn xerj");

    let output = Arc::new(Mutex::new(String::new()));
    drain_into(child.stdout.take().unwrap(), output.clone());
    drain_into(child.stderr.take().unwrap(), output.clone());

    let server = Server {
        child,
        es_port: es,
        _dir: dir,
        output,
    };

    // `/health/ready` is auth-exempt and mounted on the ES-compat router.
    let client = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{es}/health/ready");
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        if let Ok(r) = client.get(&url).send().await {
            if r.status().is_success() {
                return server;
            }
        }
        assert!(
            Instant::now() < deadline,
            "xerj never became ready on :{es}\n{}",
            server.output.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Prior-knowledge h2c handshake against the ES-compat listener, plus a handle
/// that resolves to the connection-level error (the `GOAWAY`, when there is
/// one) once the connection ends.
async fn connect(
    port: u16,
) -> (
    h2::client::SendRequest<Bytes>,
    tokio::sync::oneshot::Receiver<Option<String>>,
) {
    let tcp = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect to the ES-compat listener");
    tcp.set_nodelay(true).unwrap();
    let (client, connection) = h2::client::handshake(tcp)
        .await
        .expect("HTTP/2 handshake — the listener must speak prior-knowledge h2c");
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _ = tx.send(match connection.await {
            Ok(()) => None,
            Err(e) => Some(format!("{e} (reason: {:?})", e.reason())),
        });
    });
    (client.ready().await.expect("connection ready"), rx)
}

/// The connection-level error, or a note that the connection outlived the
/// wait. Bounded so a server that resets the *stream* without ending the
/// connection reports that instead of hanging the test.
async fn connection_error(rx: tokio::sync::oneshot::Receiver<Option<String>>) -> String {
    match tokio::time::timeout(Duration::from_secs(30), rx).await {
        Ok(Ok(Some(e))) => e,
        Ok(Ok(None)) => "the connection closed cleanly".into(),
        Ok(Err(_)) => "the connection task vanished".into(),
        Err(_) => "the connection was still open 30s later".into(),
    }
}

/// An authenticated `_bulk` whose body is chunked at 100 bytes across ~400 DATA
/// frames must return 200.
///
/// Sizing, both ways. The pre-fix budget is a fixed 25,600 bytes and a 100-byte
/// frame charges 156, so it is spent by frame 165: against `h2` 0.4.16 this
/// exact request is answered `GOAWAY ENHANCE_YOUR_CALM` mid-body, which is what
/// was watched happen before the fix went in. The post-fix budget is half the
/// 1 MiB connection window, 524,288 bytes, and ~400 frames charge at most
/// 62,556 — an eighth of it, so the pass has real margin rather than sitting on
/// a boundary. 40 KB is meanwhile an unremarkable `_bulk` by any other measure.
#[tokio::test]
async fn authenticated_bulk_chunked_at_100_bytes_completes() {
    let server = boot().await;

    let mut body = String::new();
    let mut docs = 0;
    while body.len() < 40_000 {
        body.push_str("{\"index\":{\"_index\":\"issue485\"}}\n");
        body.push_str(&format!(
            "{{\"n\":{docs},\"text\":\"a legitimate document that the client happens to stream\"}}\n"
        ));
        docs += 1;
    }
    let body = Bytes::from(body);
    let frames = body.len().div_ceil(CHUNK);

    let (mut client, conn_err) = connect(server.es_port).await;
    let request = Request::builder()
        .method("POST")
        .uri(format!("http://127.0.0.1:{}/_bulk", server.es_port))
        .header("content-type", "application/x-ndjson")
        .header("authorization", format!("ApiKey {ADMIN_KEY}"))
        .body(())
        .unwrap();
    let (response, mut send) = client.send_request(request, false).unwrap();
    send.reserve_capacity(body.len() + 1);

    // One `send_data` per DATA frame, queued back-to-back with nothing awaited
    // in between: the whole body is handed to the connection before it is
    // driven, so it goes out as one burst of ~400 small DATA frames. That is
    // the shape the issue reports — "ordinary socket writes and no artificial
    // pauses" — and it is load-bearing here. Awaiting between frames (a
    // `yield_now`, a sleep, a round trip) hands the server time to read and
    // refund each frame, and the budget is then never exhausted: with a yield
    // per frame this exact request passes on the *unfixed* stack too. A test
    // written that way proves nothing, so do not add one back.
    for (i, chunk) in body.chunks(CHUNK).enumerate() {
        if send
            .send_data(Bytes::copy_from_slice(chunk), false)
            .is_err()
        {
            let goaway = connection_error(conn_err).await;
            panic!(
                "the server cut off an authenticated {}-byte _bulk at frame {i}/{frames} \
                 (chunked at {CHUNK} bytes): {goaway}",
                body.len()
            );
        }
    }
    send.send_data(Bytes::new(), true).expect("end of stream");

    let response = match response.await {
        Ok(r) => r,
        Err(e) => {
            let goaway = connection_error(conn_err).await;
            panic!(
                "no response to an authenticated {}-byte _bulk chunked at {CHUNK} bytes \
                 ({frames} DATA frames): {e}; connection: {goaway}",
                body.len()
            );
        }
    };
    assert_eq!(
        response.status(),
        200,
        "authenticated _bulk chunked at {CHUNK} bytes answered {}",
        response.status()
    );

    let mut payload = Vec::new();
    let mut stream = response.into_body();
    while let Some(chunk) = stream.data().await {
        let chunk = chunk.expect("read the response body");
        stream.flow_control().release_capacity(chunk.len()).ok();
        payload.extend_from_slice(&chunk);
    }
    let json: serde_json::Value = serde_json::from_slice(&payload).expect("bulk response is JSON");
    assert_eq!(json["errors"], false, "{json}");
    assert_eq!(
        json["items"].as_array().map(|a| a.len()),
        Some(docs),
        "every document in the chunked body must have been indexed: {json}"
    );
}

/// The guard on the fix: the flood RUSTSEC-2026-0258 was filed about — DATA
/// frames with an empty payload, which cost the server memory and buy the
/// client nothing — must still end in `GOAWAY ENHANCE_YOUR_CALM`.
///
/// Upstream caps these with a separate counter (100 frames) rather than out of
/// the byte budget, which is why the budget could be raised for real traffic
/// without reopening this. A failure here means the protection is gone, not
/// that the fix is incomplete.
#[tokio::test]
async fn empty_data_frame_flood_is_still_refused() {
    let server = boot().await;

    let (mut client, conn_err) = connect(server.es_port).await;
    let request = Request::builder()
        .method("POST")
        .uri(format!("http://127.0.0.1:{}/_bulk", server.es_port))
        .header("content-type", "application/x-ndjson")
        .header("authorization", format!("ApiKey {ADMIN_KEY}"))
        .body(())
        .unwrap();
    let (_response, mut send) = client.send_request(request, false).unwrap();
    send.reserve_capacity(1);

    // Upstream's cap is 100 such frames. Yielding after each one lets the
    // connection task write it and read what comes back, so the client learns
    // it has been cut off within a few frames of the server deciding; without
    // that it would simply queue all 1000 and find out at the end.
    let mut cut_off_at = None;
    for i in 0..1000 {
        if send.send_data(Bytes::new(), false).is_err() {
            cut_off_at = Some(i);
            break;
        }
        tokio::task::yield_now().await;
    }
    let i = cut_off_at.expect("1000 empty DATA frames were accepted — the flood guard is gone");
    assert!(
        i < 500,
        "the empty-frame flood ran to frame {i}; upstream's cap is 100 and the client should \
         learn of it well before this"
    );

    // The teardown itself is the assertion. Which *description* of it the
    // client gets is a race — the server sends `GOAWAY ENHANCE_YOUR_CALM` and
    // then closes, and a client still writing into that socket often takes the
    // `EPIPE` first — so the reason is only checked when the client managed to
    // read it. What must never happen is a clean close or no close at all.
    let goaway = connection_error(conn_err).await;
    assert!(
        !goaway.contains("closed cleanly") && !goaway.contains("still open"),
        "the empty-frame flood ended without the connection being torn down: {goaway}"
    );
    if goaway.contains("reason: Some(") {
        assert!(
            goaway.contains("ENHANCE_YOUR_CALM"),
            "empty-frame flood was stopped at frame {i} for the wrong reason: {goaway}"
        );
    }
}
