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
/// through an mpsc channel. Outgoing messages open a fresh TCP connection per
/// send (connection pooling is a future optimisation).
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

        let transport = TcpTransport {
            node_id: node_id.clone(),
            listen_addr,
            peers: Arc::new(peers),
            secret,
            incoming: Arc::new(Mutex::new(rx)),
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

    /// Send a message to a specific peer by node ID.
    ///
    /// Opens a fresh TCP connection, completes the authenticated handshake,
    /// writes the frame, then closes.
    pub async fn send_to(&self, peer_id: &str, msg: &RaftMessage) -> Result<()> {
        let addr = self
            .peers
            .get(peer_id)
            .ok_or_else(|| anyhow::anyhow!("unknown peer: {peer_id}"))?;

        let mut stream = TcpStream::connect(addr)
            .await
            .with_context(|| format!("connect to peer {peer_id} at {addr}"))?;

        // The receiver speaks first: magic, version, challenge. Bound the whole
        // exchange so an accepting-but-silent peer cannot stall the Raft loop.
        tokio::time::timeout(SEND_TIMEOUT, async {
            let challenge = read_handshake(&mut stream).await?;
            write_hello(&mut stream, &self.secret, &challenge, &self.node_id).await?;
            write_frame(&mut stream, &self.secret, &challenge, &self.node_id, 0, msg).await
        })
        .await
        .with_context(|| format!("send to peer {peer_id} at {addr} timed out"))?
        .with_context(|| format!("send to peer {peer_id} at {addr}"))
    }
}

#[async_trait]
impl ClusterTransport for TcpTransport {
    async fn send(&self, to: &str, msg: RaftMessage) -> Result<()> {
        self.send_to(to, &msg).await
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
