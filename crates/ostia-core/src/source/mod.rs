//! Profile source providers — pluggable backends for loading bundles + profiles
//! at startup. See `spec/profile-source.md` for behavioral contracts and
//! `context/source-providers.md` for architectural rationale.
//!
//! Three providers ship in Slice 1: `file`, `http`, `postgres`. The trait
//! is async because two of the three are network-bound.

pub mod file;
pub mod http;
pub mod postgres;
pub mod profile;

pub use profile::{AuthSource, AuthSourceDef, ProfileSource, ProfileSourceDef, SourcedConfig};
