//! Adversarial tests for cluster control-frame authentication (issue #75).
//!
//! Each test drives the real `TcpTransport` listener with a hand-rolled client
//! so it can lie about the things an attacker on the cluster port would lie
//! about: no secret, the wrong secret, a replayed capture, a tampered payload.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use xerj_cluster::auth::{ClusterSecret, CHALLENGE_LEN, WIRE_MAGIC, WIRE_VERSION};
use xerj_cluster::node::ClusterTransport;
use xerj_cluster::raft::RaftMessage;
use xerj_cluster::transport::TcpTransport;

const SECRET: &str = "cluster-secret-for-tests-0123456789";
const OTHER_SECRET: &str = "a-completely-different-secret-9876";

/// How long a test waits before concluding "the listener did not deliver this".
const REJECT_WINDOW: Duration = Duration::from_millis(400);

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn free_addr() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap()
}

fn secret(s: &str) -> ClusterSecret {
    ClusterSecret::new(s).expect("valid test secret")
}

/// Start a listening transport with no peers (receive-only).
async fn listener(addr: SocketAddr, s: &str) -> TcpTransport {
    TcpTransport::new("receiver".to_string(), addr, HashMap::new(), secret(s))
        .await
        .expect("bind receiver transport")
}

fn sample_msg() -> RaftMessage {
    RaftMessage::RequestVote {
        term: 7,
        candidate_id: "attacker".to_string(),
        last_log_index: 0,
        last_log_term: 0,
    }
}

/// Connect and read the receiver's handshake, returning the challenge it issued.
async fn connect_and_handshake(addr: SocketAddr) -> (TcpStream, [u8; CHALLENGE_LEN]) {
    let mut stream = TcpStream::connect(addr).await.expect("connect");
    let mut buf = [0u8; 8 + 1 + CHALLENGE_LEN];
    stream.read_exact(&mut buf).await.expect("read handshake");
    assert_eq!(&buf[..8], WIRE_MAGIC, "handshake magic");
    assert_eq!(buf[8], WIRE_VERSION, "handshake version");
    let mut challenge = [0u8; CHALLENGE_LEN];
    challenge.copy_from_slice(&buf[9..]);
    (stream, challenge)
}

/// The authenticated hello a legitimate sender writes first.
fn hello_bytes(s: &ClusterSecret, node_id: &str, challenge: &[u8; CHALLENGE_LEN]) -> Vec<u8> {
    let mut out = Vec::new();
    let id = node_id.as_bytes();
    out.extend_from_slice(&(id.len() as u32).to_be_bytes());
    out.extend_from_slice(id);
    out.extend_from_slice(&s.hello_tag(challenge, node_id));
    out
}

/// One authenticated message frame at position `seq`.
fn frame_bytes(
    s: &ClusterSecret,
    node_id: &str,
    challenge: &[u8; CHALLENGE_LEN],
    seq: u64,
    msg: &RaftMessage,
) -> Vec<u8> {
    let payload = serde_json::to_vec(msg).unwrap();
    let mut out = Vec::new();
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&s.frame_tag(challenge, node_id, seq, &payload));
    out.extend_from_slice(&payload);
    out
}

/// Everything a legitimate sender writes for a single message: hello ‖ frame 0.
fn client_bytes(
    s: &ClusterSecret,
    node_id: &str,
    challenge: &[u8; CHALLENGE_LEN],
    msg: &RaftMessage,
) -> Vec<u8> {
    let mut out = hello_bytes(s, node_id, challenge);
    out.extend_from_slice(&frame_bytes(s, node_id, challenge, 0, msg));
    out
}

/// Assert the transport delivers nothing within the rejection window.
async fn assert_no_delivery(t: &TcpTransport, what: &str) {
    let got = tokio::time::timeout(REJECT_WINDOW, t.recv()).await;
    assert!(
        got.is_err(),
        "{what}: transport accepted a frame it must have rejected: {:?}",
        got.map(|r| r.map(|(from, m)| (from, format!("{m:?}"))))
    );
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// Baseline: two transports sharing a secret still talk to each other.
#[tokio::test]
async fn authenticated_peers_exchange_messages() {
    let addr_a = free_addr().await;
    let addr_b = free_addr().await;

    let mut peers_a = HashMap::new();
    peers_a.insert("node-b".to_string(), addr_b);

    let a = TcpTransport::new("node-a".to_string(), addr_a, peers_a, secret(SECRET))
        .await
        .unwrap();
    let b = listener(addr_b, SECRET).await;

    a.send_to("node-b", &sample_msg()).await.expect("send");

    let (from, msg) = tokio::time::timeout(Duration::from_secs(2), b.recv())
        .await
        .expect("timed out")
        .expect("recv");
    assert_eq!(from, "node-a");
    assert!(matches!(msg, RaftMessage::RequestVote { term: 7, .. }));
}

/// An unauthenticated client speaking the *old* pre-#75 wire format —
/// `[len][node_id]` then `[len][json]`, no tags — is rejected.
///
/// This is the exact shape of the issue: anyone who could reach the cluster
/// port used to be able to inject Raft control messages this way.
#[tokio::test]
async fn legacy_unauthenticated_client_is_rejected() {
    let addr = free_addr().await;
    let t = listener(addr, SECRET).await;

    let mut stream = TcpStream::connect(addr).await.unwrap();
    // The legacy client writes immediately and never reads the challenge.
    let id = b"evil-node";
    stream
        .write_all(&(id.len() as u32).to_be_bytes())
        .await
        .unwrap();
    stream.write_all(id).await.unwrap();
    let payload = serde_json::to_vec(&sample_msg()).unwrap();
    stream
        .write_all(&(payload.len() as u32).to_be_bytes())
        .await
        .unwrap();
    stream.write_all(&payload).await.unwrap();
    stream.flush().await.unwrap();

    assert_no_delivery(&t, "legacy unauthenticated client").await;
}

/// A client that completes the handshake but holds the wrong secret is rejected
/// at the hello — it never gets to send a frame that counts.
#[tokio::test]
async fn wrong_secret_is_rejected() {
    let addr = free_addr().await;
    let t = listener(addr, SECRET).await;

    let (mut stream, challenge) = connect_and_handshake(addr).await;
    let bytes = client_bytes(
        &secret(OTHER_SECRET),
        "evil-node",
        &challenge,
        &sample_msg(),
    );
    stream.write_all(&bytes).await.unwrap();
    stream.flush().await.unwrap();

    assert_no_delivery(&t, "wrong secret").await;
}

/// A frame captured off the wire cannot be replayed: the receiver issues a
/// fresh challenge per connection, so the captured tags no longer verify.
#[tokio::test]
async fn captured_frame_cannot_be_replayed() {
    let addr = free_addr().await;
    let t = listener(addr, SECRET).await;

    // Connection 1: a legitimate, correctly-signed exchange. Keep the bytes.
    let (mut stream, challenge) = connect_and_handshake(addr).await;
    let captured = client_bytes(&secret(SECRET), "node-a", &challenge, &sample_msg());
    stream.write_all(&captured).await.unwrap();
    stream.flush().await.unwrap();

    let (from, _) = tokio::time::timeout(Duration::from_secs(2), t.recv())
        .await
        .expect("legitimate frame timed out")
        .expect("recv");
    assert_eq!(from, "node-a");
    drop(stream);

    // Connection 2: replay the captured bytes verbatim.
    let (mut replay, _new_challenge) = connect_and_handshake(addr).await;
    replay.write_all(&captured).await.unwrap();
    replay.flush().await.unwrap();

    assert_no_delivery(&t, "replayed capture").await;
}

/// Swapping the payload under an otherwise-valid tag is rejected.
///
/// The substituted payload is *perfectly well-formed* Raft JSON — a vote
/// request at a much higher term, which a leader would honour by stepping
/// down — so nothing but the MAC can catch it.
#[tokio::test]
async fn payload_swapped_under_a_valid_tag_is_rejected() {
    let addr = free_addr().await;
    let t = listener(addr, SECRET).await;

    let (mut stream, challenge) = connect_and_handshake(addr).await;
    let s = secret(SECRET);

    let forged = RaftMessage::RequestVote {
        term: 9_999,
        candidate_id: "attacker".to_string(),
        last_log_index: 0,
        last_log_term: 0,
    };
    let forged_payload = serde_json::to_vec(&forged).unwrap();
    // The tag the attacker captured — computed over the *original* payload.
    let stolen_tag = s.frame_tag(
        &challenge,
        "node-a",
        0,
        &serde_json::to_vec(&sample_msg()).unwrap(),
    );

    let mut bytes = hello_bytes(&s, "node-a", &challenge);
    bytes.extend_from_slice(&(forged_payload.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&stolen_tag);
    bytes.extend_from_slice(&forged_payload);

    stream.write_all(&bytes).await.unwrap();
    stream.flush().await.unwrap();

    assert_no_delivery(&t, "payload swapped under a stolen tag").await;
}

/// A second frame replayed within the same connection is rejected: `seq` is
/// part of the signed material, so frame 0 cannot be re-sent as frame 1.
#[tokio::test]
async fn duplicated_frame_within_connection_is_rejected() {
    let addr = free_addr().await;
    let t = listener(addr, SECRET).await;

    let (mut stream, challenge) = connect_and_handshake(addr).await;
    let s = secret(SECRET);
    let frame0 = frame_bytes(&s, "node-a", &challenge, 0, &sample_msg());

    // Frame 0 (valid) followed by a byte-identical copy (which lands on seq 1).
    stream
        .write_all(&hello_bytes(&s, "node-a", &challenge))
        .await
        .unwrap();
    stream.write_all(&frame0).await.unwrap();
    stream.write_all(&frame0).await.unwrap();
    stream.flush().await.unwrap();

    // The first frame is legitimate and must arrive.
    let (from, _) = tokio::time::timeout(Duration::from_secs(2), t.recv())
        .await
        .expect("first frame timed out")
        .expect("recv");
    assert_eq!(from, "node-a");

    // The duplicate must not.
    assert_no_delivery(&t, "duplicated frame").await;
}

/// A peer that connects and then goes silent must not pin the accept task
/// forever, and must never be treated as authenticated.
#[tokio::test]
async fn silent_client_delivers_nothing() {
    let addr = free_addr().await;
    let t = listener(addr, SECRET).await;

    let (_stream, _challenge) = connect_and_handshake(addr).await;
    assert_no_delivery(&t, "silent client").await;
}
