//! Binary-source schema: the YAML/JSONB types that describe a binary and
//! its source provider.
//!
//! The shapes mirror the profile-source pattern from Slice 1: a tagged
//! `BinarySourceDef` enum discriminated on `provider:`, with provider-specific
//! fields per variant.
//!
//! Bundle `binaries:` entries are heterogeneous — a `BundleBinary` is either
//! a plain string (resolved against the registry first, then host PATH) or
//! an inline `BinaryEntry` that fully declares its source in-place.

use serde::Deserialize;

use crate::source::AuthSourceDef;

/// One entry in a bundle's `binaries:` list.
///
/// Either a plain string (`gh`) — resolved against the top-level `binaries:`
/// registry first, then host PATH for built-ins — or an inline object that
/// fully declares the binary's source, sha, and format in-place.
///
/// `serde(untagged)` so YAML/JSON accept both shapes without a discriminator.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum BundleBinary {
    /// Plain string form: looked up in the registry, then falls back to PATH.
    Name(String),
    /// Inline object form: declares everything for this binary right here.
    /// Wins over any registry entry of the same name.
    Inline(InlineBinaryEntry),
}

impl BundleBinary {
    /// The binary's name as the bundle references it. Both forms have a name.
    pub fn name(&self) -> &str {
        match self {
            BundleBinary::Name(s) => s.as_str(),
            BundleBinary::Inline(e) => &e.name,
        }
    }
}

/// Inline form of a bundle binary entry — like a `BinaryEntry` but the name
/// lives inside the object instead of being a map key.
#[derive(Debug, Clone, Deserialize)]
pub struct InlineBinaryEntry {
    pub name: String,
    pub sha256: String,
    pub format: BinaryFormat,
    #[serde(default)]
    pub entry: Option<String>,
    #[serde(default)]
    pub libs: Vec<String>,
    pub source: BinarySourceDef,
}

impl InlineBinaryEntry {
    /// Drop the name to produce a registry-equivalent entry.
    pub fn to_entry(&self) -> BinaryEntry {
        BinaryEntry {
            sha256: self.sha256.clone(),
            format: self.format,
            entry: self.entry.clone(),
            libs: self.libs.clone(),
            source: self.source.clone(),
        }
    }
}

/// A binary entry in the top-level `binaries:` registry.
///
/// Keyed by binary name in the parent map; this struct holds the rest.
/// Used both for inline-source maps in YAML/JSON config and for postgres
/// `definition` JSONB rows.
#[derive(Debug, Clone, Deserialize)]
pub struct BinaryEntry {
    pub sha256: String,
    pub format: BinaryFormat,
    /// Path inside a tarball to the entry binary. Required for `tar.gz`,
    /// ignored for `binary`.
    #[serde(default)]
    pub entry: Option<String>,
    /// Additional paths inside a tarball that should be bind-mounted alongside
    /// the entry binary (bundled shared libs).
    #[serde(default)]
    pub libs: Vec<String>,
    pub source: BinarySourceDef,
}

/// Wire format of the binary blob the source returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum BinaryFormat {
    /// A single, self-contained binary file (statically linked typically).
    /// Cached as `<root>/<sha>/<name>`.
    #[serde(rename = "binary")]
    Binary,
    /// A gzipped tar archive. Cached as `<root>/<sha>/<extracted-tree>`; the
    /// entry binary is at `<root>/<sha>/<entry_path>`.
    #[serde(rename = "tar.gz", alias = "targz")]
    TarGz,
}

/// The `source:` block on a binary entry, discriminated on `provider:`.
///
/// Mirrors `ProfileSourceDef` but with binary-specific variants. `postgres-blob`
/// reuses the profile-source's ambient connection params at runtime — operators
/// don't repeat the DSN/auth per binary.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "provider", rename_all = "kebab-case")]
pub enum BinarySourceDef {
    /// Local filesystem path — read raw bytes from `path:`.
    File { path: String },
    /// HTTP GET to `url:` with optional bearer auth.
    Http {
        url: String,
        #[serde(default)]
        auth: HttpAuthDef,
    },
    /// `SELECT <value_column> FROM <table> WHERE <key_column> = $1` — bytes
    /// live in a BYTEA column. Uses the ambient `profile_source` postgres
    /// connection.
    PostgresBlob {
        table: String,
        key_column: String,
        value_column: String,
        key: String,
    },
}

/// Auth for an HTTP binary source. Subset of `AuthSourceDef` — only `none`
/// and `static_secret` (bearer) are meaningful for Slice 3.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(untagged)]
pub enum HttpAuthDef {
    /// No `auth:` block on the source.
    #[default]
    Absent,
    /// Reuses the profile-source auth schema for consistency. Only `none` and
    /// `static_secret { bearer_env }` are honored.
    Inherited(AuthSourceDef),
}

/// A binary fully resolved for sandbox bind-mounting.
///
/// Produced by `resolve_profile_with_identity` after walking each bundle's
/// heterogeneous `binaries:` list against the registry, host PATH, or inline
/// declarations.
#[derive(Debug, Clone)]
pub enum ResolvedBinaryRef {
    /// Cache-managed: bind-mount the file at `cache_path` into the sandbox
    /// as `/usr/bin/<name>`. The binary's sha and entry/libs come from a
    /// `BinaryEntry`. Tarballs land their entry here too — `cache_path` is
    /// the extracted file path.
    Cached {
        name: String,
        sha256: String,
        entry: BinaryEntry,
    },
    /// Fall-through: not in the registry, not inline — resolve via host PATH
    /// (today's `which` + goblin behavior).
    HostPath { name: String },
}

impl ResolvedBinaryRef {
    pub fn name(&self) -> &str {
        match self {
            ResolvedBinaryRef::Cached { name, .. } | ResolvedBinaryRef::HostPath { name } => name,
        }
    }
}
