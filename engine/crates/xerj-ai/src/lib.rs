//! # xerj-ai
//!
//! AI-native features for the xerj search engine.
//!
//! Provides:
//! - [`embed`]   — Embedding proxy: async HTTP client for OpenAI-compatible embedding APIs
//! - [`local`]   — Built-in zero-config deterministic text embedder (feature hashing)
//! - [`chunker`] — Text chunking with sentence-aware splitting and overlap
//! - [`memory`]  — Agent memory: semantic dedup + recency-blended recall

pub mod chunker;
pub mod embed;
pub mod local;
pub mod memory;

pub use chunker::{Chunk, TextChunker};
pub use embed::{EmbeddingProxy, EmbeddingProxyConfig};
pub use local::{local_embed, DEFAULT_DIMS};
pub use memory::{AgentMemory, MemoryEntry};

pub use xerj_common::Result;
