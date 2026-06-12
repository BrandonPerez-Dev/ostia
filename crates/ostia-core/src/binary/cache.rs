//! Content-addressed binary cache.
//!
//! Binaries land at `<root>/<sha256>/<name>` for `Binary` format, or unpacked
//! at `<root>/<sha256>/<extracted-tree>` for `TarGz`. Two binaries with the
//! same sha share a path regardless of name; two with different shas never
//! collide.
//!
//! `stage()` verifies sha BEFORE writing the final cache file (atomic-ish:
//! we write to a temp path, sha-check, then rename). Tar extraction
//! path-sanitizes every entry — rejecting absolute paths, `..` components,
//! and entry types other than regular files / directories.

use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use super::schema::{BinaryEntry, BinaryFormat};

/// Compute the lowercase-hex sha256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let result = hasher.finalize();
    let mut out = String::with_capacity(64);
    for b in result {
        use std::fmt::Write;
        let _ = write!(out, "{:02x}", b);
    }
    out
}

/// On-disk cache for binary blobs. The cache is the only place ostia
/// bind-mounts binaries from at sandbox setup time.
#[derive(Debug, Clone)]
pub struct BinaryCache {
    root: PathBuf,
}

impl BinaryCache {
    /// Create or open a cache at `root`. Creates the directory tree if
    /// missing. Returns an error on permission denied or read-only filesystem.
    pub fn new<P: Into<PathBuf>>(root: P) -> anyhow::Result<Self> {
        let root: PathBuf = root.into();
        fs::create_dir_all(&root).map_err(|e| {
            anyhow::anyhow!(
                "binary cache: failed to create cache dir `{}`: {}",
                root.display(),
                e
            )
        })?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path where the *entry* binary will live on disk after caching.
    /// For `Binary` format: `<root>/<sha>/<name>`.
    /// For `TarGz` format: `<root>/<sha>/<entry-path-inside-tar>`.
    pub fn entry_path(&self, sha: &str, name: &str, entry: &BinaryEntry) -> PathBuf {
        match entry.format {
            BinaryFormat::Binary => self.root.join(sha).join(name),
            BinaryFormat::TarGz => {
                let entry_path = entry
                    .entry
                    .as_deref()
                    .unwrap_or("");
                self.root.join(sha).join(entry_path)
            }
        }
    }

    /// Is the entry binary cached on disk? (Checks existence of `entry_path`.)
    pub fn is_cached(&self, sha: &str, name: &str, entry: &BinaryEntry) -> bool {
        self.entry_path(sha, name, entry).exists()
    }

    /// Compute additional library file paths inside the cache that a tarball
    /// declares via `libs:`. For non-tarball entries, returns empty.
    pub fn lib_paths(&self, sha: &str, entry: &BinaryEntry) -> Vec<PathBuf> {
        if !matches!(entry.format, BinaryFormat::TarGz) {
            return Vec::new();
        }
        entry
            .libs
            .iter()
            .map(|l| self.root.join(sha).join(l))
            .collect()
    }

    /// Verify `bytes`' sha matches `entry.sha256` and stage them on disk.
    /// Returns the cache path the entry binary now lives at.
    ///
    /// For `Binary`, writes `<root>/<sha>/<name>` (mode 0o755).
    /// For `TarGz`, extracts the archive under `<root>/<sha>/`. Extraction
    /// rejects unsafe paths (absolute, `..`, links).
    pub fn stage(&self, name: &str, entry: &BinaryEntry, bytes: &[u8]) -> anyhow::Result<PathBuf> {
        let actual_sha = sha256_hex(bytes);
        if !actual_sha.eq_ignore_ascii_case(&entry.sha256) {
            anyhow::bail!(
                "binary `{}`: sha256 mismatch / integrity check failed: claimed `{}`, computed `{}`",
                name,
                entry.sha256,
                actual_sha
            );
        }

        let sha = entry.sha256.to_ascii_lowercase();
        let dir = self.root.join(&sha);
        fs::create_dir_all(&dir).map_err(|e| {
            anyhow::anyhow!(
                "binary `{}`: failed to create cache dir `{}`: {}",
                name,
                dir.display(),
                e
            )
        })?;

        match entry.format {
            BinaryFormat::Binary => {
                let path = dir.join(name);
                write_executable(&path, bytes).map_err(|e| {
                    anyhow::anyhow!(
                        "binary `{}`: failed to stage to `{}`: {}",
                        name,
                        path.display(),
                        e
                    )
                })?;
                Ok(path)
            }
            BinaryFormat::TarGz => {
                let entry_rel = entry
                    .entry
                    .as_deref()
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "binary `{}`: format `tar.gz` requires an `entry:` field naming the binary inside the archive",
                            name
                        )
                    })?;
                extract_tar_gz(&dir, bytes).map_err(|e| {
                    anyhow::anyhow!(
                        "binary `{}`: failed to extract tar.gz into `{}`: {}",
                        name,
                        dir.display(),
                        e
                    )
                })?;
                let entry_path = dir.join(entry_rel);
                if !entry_path.exists() {
                    anyhow::bail!(
                        "binary `{}`: extracted tarball missing entry path `{}`",
                        name,
                        entry_rel
                    );
                }
                Ok(entry_path)
            }
        }
    }
}

fn write_executable(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Write to a sibling temp, fsync-ish (rename), set exec mode.
    let tmp_path = path.with_extension("partial");
    {
        let mut f = fs::File::create(&tmp_path)?;
        f.write_all(bytes)?;
        f.sync_all().ok();
    }
    set_executable(&tmp_path)?;
    fs::rename(&tmp_path, path)?;
    Ok(())
}

#[cfg(unix)]
fn set_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Extract a `.tar.gz` payload into `dest_dir`, with path sanitization.
fn extract_tar_gz(dest_dir: &Path, gz_bytes: &[u8]) -> std::io::Result<()> {
    use flate2::read::GzDecoder;

    let mut decoder = GzDecoder::new(gz_bytes);
    let mut tar_bytes = Vec::new();
    decoder.read_to_end(&mut tar_bytes)?;

    let mut archive = tar::Archive::new(tar_bytes.as_slice());
    for entry_result in archive.entries()? {
        let mut entry = entry_result?;
        let header_kind = entry.header().entry_type();
        let raw_path = entry.path()?.into_owned();

        // Path sanitization — reject anything we can't safely place
        // under dest_dir.
        if !is_safe_relative_path(&raw_path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "tarball contains unsafe path `{}` (absolute, `..`, or escaping)",
                    raw_path.display()
                ),
            ));
        }

        let target = dest_dir.join(&raw_path);

        match header_kind {
            tar::EntryType::Directory => {
                fs::create_dir_all(&target)?;
            }
            tar::EntryType::Regular => {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut out = fs::File::create(&target)?;
                std::io::copy(&mut entry, &mut out)?;
                let mode = entry.header().mode().unwrap_or(0o644);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let perms = fs::Permissions::from_mode(mode);
                    fs::set_permissions(&target, perms)?;
                }
                #[cfg(not(unix))]
                {
                    let _ = mode;
                }
            }
            // Reject anything else: symlink, hardlink, char dev, block dev,
            // fifo, etc. Slice 3 only supports regular files + dirs in
            // tarballs — defensive against path-traversal via symlinks.
            other => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!(
                        "tarball entry `{}` has unsupported type `{:?}`",
                        raw_path.display(),
                        other
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// True if `p` is relative AND does not climb above its parent (no `..`).
fn is_safe_relative_path(p: &Path) -> bool {
    for c in p.components() {
        match c {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::binary::schema::{BinaryFormat, BinarySourceDef};

    fn binary_entry(sha: &str) -> BinaryEntry {
        BinaryEntry {
            sha256: sha.to_string(),
            format: BinaryFormat::Binary,
            entry: None,
            libs: vec![],
            source: BinarySourceDef::File {
                path: "/dev/null".into(),
            },
        }
    }

    #[test]
    fn sha256_mismatch_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = BinaryCache::new(tmp.path()).unwrap();
        let entry = binary_entry("0".repeat(64).as_str());
        let result = cache.stage("foo", &entry, b"hello");
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("sha256"), "got: {}", err);
    }

    #[test]
    fn binary_format_lands_at_sha_name() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = BinaryCache::new(tmp.path()).unwrap();
        let sha = sha256_hex(b"hi");
        let entry = binary_entry(&sha);
        let path = cache.stage("foo", &entry, b"hi").unwrap();
        assert_eq!(path, cache.entry_path(&sha, "foo", &entry));
        assert!(path.exists());
        let written = std::fs::read(&path).unwrap();
        assert_eq!(written, b"hi");
    }

    #[test]
    fn unsafe_paths_rejected() {
        assert!(!is_safe_relative_path(Path::new("/etc/passwd")));
        assert!(!is_safe_relative_path(Path::new("../etc/passwd")));
        assert!(!is_safe_relative_path(Path::new("a/../../b")));
        assert!(is_safe_relative_path(Path::new("bin/jq")));
        assert!(is_safe_relative_path(Path::new("./bin/jq")));
    }
}
