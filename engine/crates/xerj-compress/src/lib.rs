//! # xerj-compress
//!
//! Compression engine for the xerj search engine.
//!
//! Provides:
//! - [`codec`] — Codec trait with LZ4, Zstd, and passthrough implementations
//! - [`block`] — Block-level compression for 128-document segments
//! - [`dictionary`] — Dictionary encoding for repeated string values
//! - [`field_codec`] — Intelligent automatic field encoding engine

pub mod block;
pub mod codec;
pub mod dictionary;
pub mod field_codec;

pub use block::{BlockReader, BlockWriter};
pub use codec::{get_codec, Codec, CompressionLevel, Lz4Codec, NoneCodec, ZstdCodec};
pub use dictionary::{DictionaryDecoder, DictionaryEncoder};
pub use field_codec::{FieldAnalyzer, FieldEncoding, TimestampFormat};

pub use xerj_common::Result;
