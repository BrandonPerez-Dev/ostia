//! File-backed profile source.
//!
//! Reads a YAML file at the configured path and parses its `bundles:` +
//! `profiles:` top-level blocks. The file's path is resolved as given —
//! relative paths are relative to the working directory of `ostia serve`.

use std::collections::HashMap;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;

use crate::config::{Bundle, ProfileDef};

use super::profile::{ProfileSource, SourcedConfig};

/// Loader for a profile config stored as YAML on disk.
pub struct FileProfileSource {
    path: PathBuf,
}

impl FileProfileSource {
    pub fn new<P: Into<PathBuf>>(path: P) -> Self {
        Self { path: path.into() }
    }
}

/// The subset of `OstiaConfig` we read out of the source file.
///
/// The source provides operational content (bundles + profiles). Bootstrap
/// concerns (`profile_source`, `endpoints`, `auth`) stay on the outer
/// `--config` file and are NOT read from here.
#[derive(Debug, Deserialize, Default)]
pub(crate) struct SourceContent {
    #[serde(default)]
    pub bundles: HashMap<String, Bundle>,
    #[serde(default)]
    pub profiles: HashMap<String, ProfileDef>,
}

impl From<SourceContent> for SourcedConfig {
    fn from(s: SourceContent) -> Self {
        SourcedConfig {
            bundles: s.bundles,
            profiles: s.profiles,
        }
    }
}

#[async_trait]
impl ProfileSource for FileProfileSource {
    async fn load(&self) -> anyhow::Result<SourcedConfig> {
        let contents = std::fs::read_to_string(&self.path).map_err(|e| {
            anyhow::anyhow!(
                "profile source (file): failed to read `{}`: {}",
                self.path.display(),
                e
            )
        })?;
        let parsed: SourceContent = serde_yaml::from_str(&contents).map_err(|e| {
            anyhow::anyhow!(
                "profile source (file): failed to parse YAML at `{}`: {}",
                self.path.display(),
                e
            )
        })?;
        Ok(parsed.into())
    }
}
