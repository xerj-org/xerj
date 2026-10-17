//! Corpus Hub v2 client (discussion #1374, `docs/CORPUS_HUB_V2_IMPLEMENTATION.md`).
//!
//! The hub serves PRE-INDEXED packs. This crate pulls one, restores it into a
//! local node with no re-indexing, routes a question to the right corpus, and
//! answers from the local pack when it is cached or through the hosted hub when
//! it is not — so an agent gets an answer in one step instead of the nine the v1
//! clone-and-index path needed.
//!
//! The data shapes that cross crates live in [`xerj_common::hub`]; this crate
//! re-exports them so callers write `xerj_hub::Answer`, not the full path.
//!
//! Modules are added step by step per the implementation plan:
//! pack build/pull (Step 2), definitions (Step 3), catalogue + sketch (Step 4),
//! router + ask (Step 5). Alongside them this file re-exports the shared types.

pub mod ask;
pub mod build_catalogue;
pub mod catalogue;
pub mod cli;
pub mod def;
mod flock;
pub mod lockfile;
pub mod node;
pub mod pack;
pub mod router;
pub mod sketch;
pub mod sync;
#[cfg(test)]
mod test_support;

pub use xerj_common::hub::{
    render, Answer, AnswerMode, AnsweredFrom, Catalogue, CatalogueEntry, DefRecord, Hit,
    LicenceClass, PackIndex, PackManifest, PackSource, Sketch, AUTO_PULL_MAX_BYTES,
    CATALOGUE_FORMAT_VERSION, DEFAULT_HUB_URL, DISK_BUDGET_BYTES, HUB_PUBKEY_HEX,
    PACK_FORMAT_VERSION,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reexports_resolve() {
        // The crate compiles and the shared types are reachable under xerj_hub::.
        assert_eq!(PACK_FORMAT_VERSION, 1);
        assert_eq!(AnsweredFrom::LocalPack.label(), "local pack");
        let _ = LicenceClass::AdaptWithAttribution;
    }
}
