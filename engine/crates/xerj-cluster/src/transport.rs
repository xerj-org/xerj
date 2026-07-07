//! TCP-based transport for inter-node communication.
//!
//! Wire format: `[4-byte big-endian length][JSON-serialized RaftMessage]`
//!
//! Each connection carries messages from one peer to this node. The sender
//! writes length-prefixed JSON frames; the receiver decodes them and pushes
//! them onto the shared `incoming` channel.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};

use crate::node::ClusterTransport;
use crate::raft::RaftMessage;

// ── Wire helpers ──────────────────────────────────────────────────────────────

/// Write a single length-prefixed JSON frame to the given stream.
async fn write_frame(stream: &mut TcpStream, msg: &RaftMessage) -> Result<()> {
    let payload = serde_json::to_vec(msg).context("serialize RaftMessage")?;
    let len = payload.len() as u32;
    stream
        .write_all(&len.to_be_bytes())
        .await
        .context("write frame length")?;
    stream
        .write_all(&payload)
        .await
        .context("write frame payload")?;
    stream.flush().await.context("flush frame")?;
    Ok(())
}

/// Read a single length-prefixed JSON frame from the given stream.
async fn read_frame(stream: &mut TcpStream) -> Result<RaftMessage> {
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .context("read frame length")?;
    let len = u32::from_be_bytes(len_buf) as usize;

    // Guard against absurdly large frames (e.g. 10 MiB max).
    if len > 10 * 1024 * 1024 {
        anyhow::bail!("frame too large: {} bytes", len);
    }

    let mut payload = vec![0u8; len];
    stream
        .read_exact(&mut payload)
        .await
        .context("read frame payload")?;
    let msg: RaftMessage = serde_json::from_slice(&payload).context("deserialize RaftMessage")?;
    Ok(msg)
}

// ── TcpTransport ─────────────────────────────────────────────────────────────

/// TCP-based transport for inter-node communication.
///
/// Wire format: `[4-byte big-endian length][JSON-serialized RaftMessage]`
///
/// Incoming messages arrive via a background listener task and are delivered
/// through an mpsc channel. Outgoing messages open a fresh TCP connection per
/// send (connection pooling is a future optimisation).
pub struct TcpTransport {
    /// This node's identifier.
    pub node_id: String,
    /// Address on which this node listens.
    listen_addr: SocketAddr,
    /// Map of peer node_id → socket address.
    peers: Arc<HashMap<String, SocketAddr>>,
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
    /// The listener task is spawned immediately; call [`start`] is not needed
    /// (this constructor already binds and spawns).
    pub async fn new(
        node_id: String,
        listen_addr: SocketAddr,
        peers: HashMap<String, SocketAddr>,
    ) -> Result<Self> {
        let (tx, rx) = mpsc::channel::<(String, RaftMessage)>(1024);

        let transport = TcpTransport {
            node_id: node_id.clone(),
            listen_addr,
            peers: Arc::new(peers),
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
        info!(node = %node_id, addr = %self.listen_addr, "TCP transport listening");

        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, peer_addr)) => {
                        debug!(node = %node_id, %peer_addr, "Incoming TCP connection");
                        let tx2 = tx.clone();
                        let nid = node_id.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, nid.clone(), tx2).await {
                                debug!(node = %nid, error = %e, "TCP connection closed");
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
    /// Opens a fresh TCP connection, writes the frame, then closes.
    pub async fn send_to(&self, peer_id: &str, msg: &RaftMessage) -> Result<()> {
        let addr = self
            .peers
            .get(peer_id)
            .ok_or_else(|| anyhow::anyhow!("unknown peer: {peer_id}"))?;

        let mut stream = TcpStream::connect(addr)
            .await
            .with_context(|| format!("connect to peer {peer_id} at {addr}"))?;

        // Send our node_id as a header frame so the receiver knows who sent this.
        let header = self.node_id.as_bytes();
        let hlen = header.len() as u32;
        stream.write_all(&hlen.to_be_bytes()).await?;
        stream.write_all(header).await?;

        write_frame(&mut stream, msg).await?;
        Ok(())
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

/// Handle a single inbound TCP connection: read the sender ID header, then
/// drain all frames and push them into the shared channel.
async fn handle_connection(
    mut stream: TcpStream,
    _local_node_id: String,
    tx: mpsc::Sender<(String, RaftMessage)>,
) -> Result<()> {
    // Read sender header: [4-byte len][node_id bytes]
    let mut hlen_buf = [0u8; 4];
    stream
        .read_exact(&mut hlen_buf)
        .await
        .context("read sender header length")?;
    let hlen = u32::from_be_bytes(hlen_buf) as usize;
    if hlen > 256 {
        anyhow::bail!("sender header too large: {} bytes", hlen);
    }
    let mut hbuf = vec![0u8; hlen];
    stream
        .read_exact(&mut hbuf)
        .await
        .context("read sender header")?;
    let from = String::from_utf8(hbuf).context("decode sender node_id")?;

    // Read all message frames from this connection.
    // Loop exits on EOF or parse error (read_frame returns Err).
    while let Ok(msg) = read_frame(&mut stream).await {
        if tx.send((from.clone(), msg)).await.is_err() {
            break; // receiver dropped
        }
    }

    Ok(())
}
