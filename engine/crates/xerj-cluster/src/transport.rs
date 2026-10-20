//! TCP-based transport for inter-node communication.
//!
//! Every connection is authenticated with the cluster-wide shared secret
//! (HMAC-SHA256, see [`crate::auth`]). There is no unauthenticated mode: the
//! transport cannot be constructed without a validated [`ClusterSecret`].
//!
//! ## Wire format (version 1)
//!
//! ```text
//! receiver → sender   (handshake, sent immediately on accept)
//!   [8]  magic "XERJCLUS"
//!   [1]  wire version (1)
//!   [32] challenge — fresh random bytes, per connection
//!
//! sender → receiver   (hello, exactly once)
//!   [4]  node_id length (u32 BE, ≤ 256)
//!   [n]  node_id (UTF-8)
//!   [32] HMAC tag over (hello-context, version, challenge, node_id)
//!
//! sender → receiver   (message frame, repeated until EOF)
//!   [4]  payload length (u32 BE, ≤ 10 MiB)
//!   [32] HMAC tag over (frame-context, version, challenge, node_id, seq, payload)
//!   [n]  payload — JSON-serialised RaftMessage
//! ```
//!
//! `seq` is the frame's zero-based index within the connection and is never
//! transmitted; both ends count it independently.
//!
//! A frame's tag is verified **before** its payload is handed to the JSON
//! deserialiser, so unauthenticated bytes never reach `serde_json`.
//!
//! ## Wire compatibility
//!
//! This framing is **not** compatible with the pre-authentication format. A
//! node running this version cannot talk to a node running an older one, in
//! either direction: the old sender writes a JSON frame where the new receiver
//! expects to write a challenge first, and the new sender waits for a handshake
//! an old receiver never sends. Upgrading a cluster requires a full stop/start,
//! not a rolling restart.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use rand::RngCore;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};

use crate::auth::{tags_match, ClusterSecret, CHALLENGE_LEN, TAG_LEN, WIRE_MAGIC, WIRE_VERSION};
use crate::node::ClusterTransport;
use crate::raft::RaftMessage;

/// Largest accepted frame payload (10 MiB).
const MAX_FRAME_BYTES: usize = 10 * 1024 * 1024;

/// Largest accepted node-id length in the hello header.
const MAX_NODE_ID_BYTES: usize = 256;

/// How long the receiver will wait for a connected peer to complete its hello.
///
/// Without this, a peer that connects and then goes silent pins an accept task
/// (and its buffers) indefinitely — cheap for an attacker, expensive for us.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a send may take once the TCP connection is established.
///
/// Authentication makes the sender *read* before it writes (it needs the
/// receiver's challenge), so a peer that accepts connections and then goes
/// silent could otherwise stall the Raft loop indefinitely. The whole
/// post-connect exchange is bounded instead.
const SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// Outbound messages buffered per peer before new ones are dropped (#1168).
///
/// A peer that has not drained its queue in this many messages (hours of
/// heartbeats at the 50 ms rate) is down; Raft retransmits by design — the
/// leader's next AppendEntries resends everything from `next_index` — so
/// dropping is safe where buffering is not.
const OUTBOUND_QUEUE_CAPACITY: usize = 256;

/// Backoff before the next attempt to a peer after `failures` consecutive
/// failed sends: 100 ms, 200, 400, … capped at 6.4 s (#1168).
///
/// Public so the policy is testable from the integration tests: a dead peer
/// costs one immediate attempt, a handful in the first second, then at most
/// one per 6.4 s — never the 20 attempts/s of the pre-fix per-tick retry,
/// and never a `WARN` per tick.
pub fn backoff_after_failures(failures: u32) -> Duration {
    const BASE_MS: u64 = 100;
    const CAP_MS: u64 = 6_400;
    let shift = failures.saturating_sub(1).min(6);
    Duration::from_millis((BASE_MS << shift).min(CAP_MS))
}

/// Length of the receiver's handshake: magic ‖ version ‖ challenge.
const HANDSHAKE_LEN: usize = WIRE_MAGIC.len() + 1 + CHALLENGE_LEN;

// ── Wire helpers ──────────────────────────────────────────────────────────────

/// Write the receiver-side handshake and return the challenge it carried.
async fn write_handshake(stream: &mut TcpStream) -> Result<[u8; CHALLENGE_LEN]> {
    let mut challenge = [0u8; CHALLENGE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut challenge);

    let mut buf = [0u8; HANDSHAKE_LEN];
    buf[..WIRE_MAGIC.len()].copy_from_slice(WIRE_MAGIC);
    buf[WIRE_MAGIC.len()] = WIRE_VERSION;
    buf[WIRE_MAGIC.len() + 1..].copy_from_slice(&challenge);

    stream.write_all(&buf).await.context("write handshake")?;
    stream.flush().await.context("flush handshake")?;
    Ok(challenge)
}

/// Read and validate the receiver's handshake, returning the challenge.
async fn read_handshake(stream: &mut TcpStream) -> Result<[u8; CHALLENGE_LEN]> {
    let mut buf = [0u8; HANDSHAKE_LEN];
    stream
        .read_exact(&mut buf)
        .await
        .context("read handshake")?;

    if &buf[..WIRE_MAGIC.len()] != WIRE_MAGIC {
        anyhow::bail!("peer did not send a xerj cluster handshake (bad magic)");
    }
    let version = buf[WIRE_MAGIC.len()];
    if version != WIRE_VERSION {
        anyhow::bail!(
            "cluster wire version mismatch: peer speaks v{version}, this node speaks v{WIRE_VERSION}"
        );
    }

    let mut challenge = [0u8; CHALLENGE_LEN];
    challenge.copy_from_slice(&buf[WIRE_MAGIC.len() + 1..]);
    Ok(challenge)
}

/// Write the authenticated hello identifying this node.
async fn write_hello(
    stream: &mut TcpStream,
    secret: &ClusterSecret,
    challenge: &[u8; CHALLENGE_LEN],
    node_id: &str,
) -> Result<()> {
    let id = node_id.as_bytes();
    if id.len() > MAX_NODE_ID_BYTES {
        anyhow::bail!("node_id too long: {} bytes", id.len());
    }
    let tag = secret.hello_tag(challenge, node_id);

    stream
        .write_all(&(id.len() as u32).to_be_bytes())
        .await
        .context("write hello length")?;
    stream.write_all(id).await.context("write hello node_id")?;
    stream.write_all(&tag).await.context("write hello tag")?;
    stream.flush().await.context("flush hello")?;
    Ok(())
}

/// Read and authenticate the peer's hello, returning its node id.
async fn read_hello(
    stream: &mut TcpStream,
    secret: &ClusterSecret,
    challenge: &[u8; CHALLENGE_LEN],
) -> Result<String> {
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .context("read hello length")?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_NODE_ID_BYTES {
        anyhow::bail!("hello node_id too large: {len} bytes");
    }

    let mut id_buf = vec![0u8; len];
    stream
        .read_exact(&mut id_buf)
        .await
        .context("read hello node_id")?;
    let mut tag = [0u8; TAG_LEN];
    stream
        .read_exact(&mut tag)
        .await
        .context("read hello tag")?;

    // Authenticate the raw bytes before trusting them as a UTF-8 node id.
    let from = String::from_utf8(id_buf).context("decode hello node_id")?;
    let expected = secret.hello_tag(challenge, &from);
    if !tags_match(&expected, &tag) {
        anyhow::bail!("cluster authentication failed: bad hello tag");
    }
    Ok(from)
}

/// Write a single authenticated frame.
async fn write_frame(
    stream: &mut TcpStream,
    secret: &ClusterSecret,
    challenge: &[u8; CHALLENGE_LEN],
    node_id: &str,
    seq: u64,
    msg: &RaftMessage,
) -> Result<()> {
    let payload = serde_json::to_vec(msg).context("serialize RaftMessage")?;
    if payload.len() > MAX_FRAME_BYTES {
        anyhow::bail!("frame too large to send: {} bytes", payload.len());
    }
    let tag = secret.frame_tag(challenge, node_id, seq, &payload);

    stream
        .write_all(&(payload.len() as u32).to_be_bytes())
        .await
        .context("write frame length")?;
    stream.write_all(&tag).await.context("write frame tag")?;
    stream
        .write_all(&payload)
        .await
        .context("write frame payload")?;
    stream.flush().await.context("flush frame")?;
    Ok(())
}

/// Read, authenticate, and decode a single frame.
///
/// Returns `Ok(None)` on a clean end of stream (the peer closed after its last
/// frame, which is the normal case for the connection-per-send sender).
///
/// The tag is verified before the payload is deserialised, so a forged or
/// tampered frame never reaches `serde_json`.
async fn read_frame(
    stream: &mut TcpStream,
    secret: &ClusterSecret,
    challenge: &[u8; CHALLENGE_LEN],
    from: &str,
    seq: u64,
) -> Result<Option<RaftMessage>> {
    let mut len_buf = [0u8; 4];
    match stream.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(anyhow::Error::new(e).context("read frame length")),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        anyhow::bail!("frame too large: {len} bytes");
    }

    let mut tag = [0u8; TAG_LEN];
    stream
        .read_exact(&mut tag)
        .await
        .context("read frame tag")?;
    let mut payload = vec![0u8; len];
    stream
        .read_exact(&mut payload)
        .await
        .context("read frame payload")?;

    let expected = secret.frame_tag(challenge, from, seq, &payload);
    if !tags_match(&expected, &tag) {
        anyhow::bail!("cluster authentication failed: bad frame tag (seq {seq})");
    }

    let msg = serde_json::from_slice(&payload).context("deserialize RaftMessage")?;
    Ok(Some(msg))
}

// ── TcpTransport ─────────────────────────────────────────────────────────────

/// TCP-based transport for inter-node communication.
///
/// Incoming messages arrive via a background listener task and are delivered
/// through an mpsc channel. Outgoing messages are **enqueued** per peer — one
/// bounded queue and one long-lived sender task per peer — so the Raft tick
/// loop never awaits a peer's TCP I/O (#1168). Each attempt still opens a
/// fresh connection (connection pooling is a future optimisation).
///
/// Failure semantics of the outbound path, by design: a send that fails or a
/// queue that is full **drops the message** and backs off
/// ([`backoff_after_failures`]); it never blocks, retries synchronously, or
/// buffers unboundedly. Raft tolerates the drops — heartbeats and log
/// replication are retransmitted from `next_index` on the next tick, and a
/// vote that arrives late is simply ignored. The pre-fix behaviour (inline
/// `await` per send, bounded only by [`SEND_TIMEOUT`]) let one dead peer
/// consume the full 5 s of every heartbeat round while the election timeout
/// is 150–300 ms: the live peers starved, and a 3-node ring killed at its
/// leader churned through a new election every 1.5–11 s indefinitely.
///
/// Every connection is authenticated in both directions of setup: the receiver
/// proves nothing (it holds no identity beyond the secret) but issues a
/// challenge, and the sender proves knowledge of the shared secret on the hello
/// and on every frame.
pub struct TcpTransport {
    /// This node's identifier.
    pub node_id: String,
    /// Address on which this node listens.
    listen_addr: SocketAddr,
    /// Map of peer node_id → socket address.
    peers: Arc<HashMap<String, SocketAddr>>,
    /// Cluster-wide shared secret used to authenticate every frame.
    secret: ClusterSecret,
    /// Receives `(sender_node_id, msg)` from the background listener.
    incoming: Arc<Mutex<mpsc::Receiver<(String, RaftMessage)>>>,
    /// One outbound queue per peer, drained by [`peer_sender_task`]. Built
    /// once in [`TcpTransport::new`] — the peer set is fixed for the
    /// transport's lifetime (membership changes arrive via the Raft log and
    /// will rebuild the transport, #1170).
    outbound: HashMap<String, mpsc::Sender<RaftMessage>>,
    // The sender half is kept alive so the channel is not closed when the
    // background listener task terminates.
    #[allow(dead_code)]
    sender: mpsc::Sender<(String, RaftMessage)>,
}

impl TcpTransport {
    /// Create a new TCP transport and begin listening for inbound connections.
    ///
    /// `secret` is the cluster-wide shared secret. It is required: there is no
    /// constructor that yields an unauthenticated transport.
    pub async fn new(
        node_id: String,
        listen_addr: SocketAddr,
        peers: HashMap<String, SocketAddr>,
        secret: ClusterSecret,
    ) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<(String, RaftMessage)>(1024);

        // A peer entry naming this node's own address is excluded (#1168).
        // The documented config convention lists the full membership on every
        // node, so a self entry is expected in the input — but a node that
        // sends its own RequestVote back to itself over loopback TCP counts
        // its own vote twice and inflates the wire message rate for nothing.
        let mut self_entry_removed = false;
        let peers: HashMap<String, SocketAddr> = peers
            .into_iter()
            .filter(|(_id, addr)| {
                if *addr == listen_addr {
                    self_entry_removed = true;
                    false
                } else {
                    true
                }
            })
            .collect();
        if self_entry_removed {
            warn!(
                node = %node_id,
                %listen_addr,
                "peer list included this node's own address — excluded (a node sending to \
                 itself double-counts its own vote)"
            );
        }

        // One sender task per peer (#1168). Spawned here, before the listener
        // binds, so a send can be enqueued the moment the transport exists.
        let mut outbound = HashMap::with_capacity(peers.len());
        for (peer_id, addr) in peers.iter() {
            let (peer_tx, peer_rx) = mpsc::channel::<RaftMessage>(OUTBOUND_QUEUE_CAPACITY);
            tokio::spawn(peer_sender_task(
                node_id.clone(),
                peer_id.clone(),
                *addr,
                secret.clone(),
                peer_rx,
            ));
            outbound.insert(peer_id.clone(), peer_tx);
        }

        let transport = TcpTransport {
            node_id: node_id.clone(),
            listen_addr,
            peers: Arc::new(peers),
            secret,
            incoming: Arc::new(Mutex::new(rx)),
            outbound,
            sender: tx.clone(),
        };

        // Spawn the background listener task.
        transport.start(tx).await?;

        Ok(transport)
    }

    /// Bind the TCP listener and spawn the accept loop.
    async fn start(&self, tx: mpsc::Sender<(String, RaftMessage)>) -> Result<()> {
        let listener = TcpListener::bind(self.listen_addr)
            .await
            .with_context(|| format!("bind TCP transport on {}", self.listen_addr))?;

        let node_id = self.node_id.clone();
        let secret = self.secret.clone();
        info!(node = %node_id, addr = %self.listen_addr, "TCP transport listening (authenticated)");

        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, peer_addr)) => {
                        debug!(node = %node_id, %peer_addr, "Incoming TCP connection");
                        let tx2 = tx.clone();
                        let nid = node_id.clone();
                        let secret = secret.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, secret, tx2).await {
                                debug!(node = %nid, %peer_addr, error = %e, "TCP connection closed");
                            }
                        });
                    }
                    Err(e) => {
                        warn!(node = %node_id, error = %e, "TCP accept error");
                    }
                }
            }
        });

        Ok(())
    }

    /// Send a message to a specific peer by node ID, awaiting the full TCP
    /// exchange.
    ///
    /// Opens a fresh TCP connection, completes the authenticated handshake,
    /// writes the frame, then closes. This is the **one-shot** path, bounded
    /// by [`SEND_TIMEOUT`] — used by tests and anywhere a synchronous
    /// delivery result is genuinely needed. The Raft loop does NOT use it:
    /// [`ClusterTransport::send`] enqueues instead (see the struct doc).
    pub async fn send_to(&self, peer_id: &str, msg: &RaftMessage) -> Result<()> {
        let addr = self
            .peers
            .get(peer_id)
            .ok_or_else(|| anyhow::anyhow!("unknown peer: {peer_id}"))?;
        send_frame(&self.node_id, peer_id, *addr, &self.secret, msg).await
    }
}

/// Open a connection to `addr` and deliver one authenticated frame.
///
/// The receiver speaks first: magic, version, challenge. The whole
/// post-connect exchange is bounded by [`SEND_TIMEOUT`] so an
/// accepting-but-silent peer cannot hold the caller indefinitely.
async fn send_frame(
    node_id: &str,
    peer_id: &str,
    addr: SocketAddr,
    secret: &ClusterSecret,
    msg: &RaftMessage,
) -> Result<()> {
    let mut stream = TcpStream::connect(addr)
        .await
        .with_context(|| format!("connect to peer {peer_id} at {addr}"))?;

    tokio::time::timeout(SEND_TIMEOUT, async {
        let challenge = read_handshake(&mut stream).await?;
        write_hello(&mut stream, secret, &challenge, node_id).await?;
        write_frame(&mut stream, secret, &challenge, node_id, 0, msg).await
    })
    .await
    .with_context(|| format!("send to peer {peer_id} at {addr} timed out"))?
    .with_context(|| format!("send to peer {peer_id} at {addr}"))
}

/// Drain one peer's outbound queue, one message per connection attempt,
/// off the Raft loop's critical path (#1168).
///
/// On failure the message is dropped and the next attempt waits
/// [`backoff_after_failures`] — Raft retransmits, so a dropped heartbeat or
/// vote response costs nothing but a tick. Logging is per **state change**
/// (`WARN` when a peer that was up goes down, `INFO` when it answers again),
/// never per attempt: the pre-fix code logged a `WARN` every tick for every
/// dead peer, which is how a 4-minute outage produced thousands of identical
/// lines. The task exits when the queue's sender half is dropped with the
/// transport.
async fn peer_sender_task(
    node_id: String,
    peer_id: String,
    addr: SocketAddr,
    secret: ClusterSecret,
    mut rx: mpsc::Receiver<RaftMessage>,
) {
    let mut failures: u32 = 0;
    let mut down = false;
    let mut retry_at: Option<std::time::Instant> = None;

    while let Some(msg) = rx.recv().await {
        if let Some(deadline) = retry_at {
            if std::time::Instant::now() < deadline {
                continue; // in backoff — drop, the next heartbeat replaces it
            }
        }
        match send_frame(&node_id, &peer_id, addr, &secret, &msg).await {
            Ok(()) => {
                failures = 0;
                retry_at = None;
                if down {
                    down = false;
                    info!(node = %node_id, peer = %peer_id, %addr, "peer reachable again");
                }
            }
            Err(e) => {
                failures += 1;
                retry_at = Some(std::time::Instant::now() + backoff_after_failures(failures));
                if !down {
                    down = true;
                    warn!(
                        node = %node_id,
                        peer = %peer_id,
                        %addr,
                        error = %e,
                        backoff_ms = backoff_after_failures(failures).as_millis() as u64,
                        "peer unreachable — Raft messages to it are dropped until it answers"
                    );
                } else {
                    debug!(node = %node_id, peer = %peer_id, %addr, error = %e, "send still failing");
                }
            }
        }
    }
}

#[async_trait]
impl ClusterTransport for TcpTransport {
    /// Enqueue `msg` on the peer's outbound queue — returns as soon as the
    /// message is accepted for delivery, never after the TCP exchange.
    ///
    /// Errors only for an unknown peer or a full queue (the message is
    /// dropped — see the struct doc for why that is the safe failure mode).
    /// Delivery failures are logged by the per-peer task, not returned here.
    async fn send(&self, to: &str, msg: RaftMessage) -> Result<()> {
        let tx = self
            .outbound
            .get(to)
            .ok_or_else(|| anyhow::anyhow!("unknown peer: {to}"))?;
        match tx.try_send(msg) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                // The peer has not drained OUTBOUND_QUEUE_CAPACITY messages —
                // it is down with the queue still backing up before the
                // backoff deadline. Drop and let retransmission cover it.
                debug!(node = %self.node_id, peer = %to, "outbound queue full — dropping Raft message");
                Ok(())
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                anyhow::bail!("peer {to} sender task is gone")
            }
        }
    }

    async fn recv(&self) -> Result<(String, RaftMessage)> {
        let mut rx = self.incoming.lock().await;
        rx.recv()
            .await
            .ok_or_else(|| anyhow::anyhow!("TCP transport incoming channel closed"))
    }
}

// ── Connection handler ────────────────────────────────────────────────────────

/// Handle a single inbound TCP connection: issue a challenge, authenticate the
/// hello, then drain authenticated frames into the shared channel.
///
/// Any authentication failure aborts the whole connection — a peer that cannot
/// produce a valid tag does not get to retry on the same socket.
async fn handle_connection(
    mut stream: TcpStream,
    secret: ClusterSecret,
    tx: mpsc::Sender<(String, RaftMessage)>,
) -> Result<()> {
    let challenge = write_handshake(&mut stream).await?;

    let from = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        read_hello(&mut stream, &secret, &challenge),
    )
    .await
    .context("peer did not complete the cluster handshake in time")??;

    // Read message frames until EOF, a decode error, or an authentication
    // failure. `seq` pins each frame to its position in the connection.
    let mut seq: u64 = 0;
    loop {
        match read_frame(&mut stream, &secret, &challenge, &from, seq).await {
            Ok(Some(msg)) => {
                if tx.send((from.clone(), msg)).await.is_err() {
                    break; // receiver dropped
                }
                seq = seq.saturating_add(1);
            }
            // Clean end of stream — the normal close for a one-frame sender.
            Ok(None) => break,
            Err(e) => {
                // A rejected frame is a security-relevant event, not routine
                // connection churn: log it loudly and drop the connection.
                warn!(peer = %from, error = %e, "cluster frame rejected");
                break;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ladder documented on [`backoff_after_failures`]: 100 ms doubling to
    /// a 6.4 s cap. Pinned here because the #1168 churn window (an election
    /// every 1.5–11 s) brackets a 5 s synchronous send timeout — the ladder's
    /// job is to make a dead peer cost ~1 attempt per 6.4 s instead of one
    /// per tick, so a change to these numbers changes the failure story.
    #[test]
    fn backoff_ladder_doubles_then_caps() {
        assert_eq!(backoff_after_failures(1), Duration::from_millis(100));
        assert_eq!(backoff_after_failures(2), Duration::from_millis(200));
        assert_eq!(backoff_after_failures(3), Duration::from_millis(400));
        assert_eq!(backoff_after_failures(4), Duration::from_millis(800));
        assert_eq!(backoff_after_failures(5), Duration::from_millis(1_600));
        assert_eq!(backoff_after_failures(6), Duration::from_millis(3_200));
        assert_eq!(backoff_after_failures(7), Duration::from_millis(6_400));
        assert_eq!(backoff_after_failures(8), Duration::from_millis(6_400));
        assert_eq!(backoff_after_failures(1_000), Duration::from_millis(6_400));
        // failures = 0 is not a state the task reaches (the first failure
        // counts as 1), but saturate rather than panic if it ever does.
        assert_eq!(backoff_after_failures(0), Duration::from_millis(100));
    }
}
