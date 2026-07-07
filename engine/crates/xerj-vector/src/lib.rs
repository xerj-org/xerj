//! # xerj-vector
//!
//! Vector search engine for the xerj search engine.
//!
//! Provides:
//! - [`distance`] — Distance metrics (Cosine, L2, DotProduct)
//! - [`hnsw`]     — Hierarchical Navigable Small World graph index
//! - [`quantizer`]— Scalar quantization for memory-efficient storage
//! - [`search`]   — High-level KNN search interface

pub mod distance;
pub mod hnsw;
pub mod quantizer;
pub mod search;

pub use distance::{compute_distance, cosine, dot_product, l2_squared, DistanceMetric};
pub use hnsw::{HnswIndex, HnswParams};
pub use quantizer::{NoneQuantizer, QuantizedData, QuantizedVectors, Quantizer, Scalar8Quantizer};
pub use search::{SearchResult, VectorSearcher};

pub use xerj_common::Result;
