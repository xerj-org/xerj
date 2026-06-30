//! # xerj-ai
//!
//! AI-native features for the xerj search engine.
//!
//! Provides:
//! - [`embed`]   — Embedding proxy: async HTTP client for OpenAI-compatible embedding APIs
//! - [`chunker`] — Text chunking with sentence-aware splitting and overlap
//! - [`memory`]  — Agent memory: semantic dedup + recency-blended recall

pub mod chunker;
pub mod embed;
pub mod memory;

pub use chunker::{Chunk, TextChunker};
pub use embed::{EmbeddingProxy, EmbeddingProxyConfig};
pub use memory::{AgentMemory, MemoryEntry};

pub use xerj_common::Result;
