//! Binary source providers — pluggable backends for fetching CLI binary bytes
//! and managing the on-disk content-addressed cache. See
//! `spec/binary-source.md` for behavioral contracts and
//! `context/source-providers.md` for architectural rationale.
//!
//! Three providers ship in Slice 3: `file`, `http`, `postgres-blob`. Binaries
//! land in a content-addressed cache at `<binary_cache_dir>/<sha256>/<name>`.

pub mod cache;
pub mod fetch;
pub mod schema;

pub use cache::BinaryCache;
pub use fetch::{fetch_bytes, BinaryFetchError, PostgresBlobParams};
pub use schema::{
    BinaryEntry, BinaryFormat, BinarySourceDef, BundleBinary, HttpAuthDef, ResolvedBinaryRef,
};
