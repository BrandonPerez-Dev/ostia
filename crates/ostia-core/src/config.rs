use serde::Deserialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::binary::{BinaryEntry, BundleBinary, ResolvedBinaryRef};

/// Server-level auth mode configuration.
#[derive(Debug, Deserialize, Clone)]
pub struct AuthModeDef {
    #[serde(default = "default_auth_mode")]
    pub mode: String,
    pub key: Option<String>,
}

fn default_auth_mode() -> String {
    "open".to_string()
}

#[derive(Debug, Deserialize)]
pub struct OstiaConfig {
    #[serde(default)]
    pub auth: Option<AuthModeDef>,
    #[serde(default)]
    pub bundles: HashMap<String, Bundle>,
    #[serde(default)]
    pub profiles: HashMap<String, ProfileDef>,
    #[serde(default)]
    pub endpoints: HashMap<String, Vec<String>>,
    /// Optional profile-source block. When present, `bundles` and `profiles`
    /// loaded from the source override whatever is inline in this file. When
    /// absent, the inline `bundles` and `profiles` are authoritative (today's
    /// behavior). See `spec/profile-source.md`.
    #[serde(default)]
    pub profile_source: Option<crate::source::ProfileSourceDef>,
    /// Optional top-level binary registry. Bundles' string-form `binaries:`
    /// entries resolve here first, then fall back to host PATH for built-ins.
    /// See `spec/binary-source.md`.
    #[serde(default)]
    pub binaries: HashMap<String, BinaryEntry>,
    /// Where the binary cache lives on disk. Defaults to
    /// `/var/lib/ostia/binaries` if absent and any registered binary is used.
    #[serde(default)]
    pub binary_cache_dir: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Bundle {
    #[serde(default)]
    pub description: Option<String>,
    /// Heterogeneous binary list. Each entry is either a plain string (resolved
    /// against the top-level `binaries:` registry, then host PATH) or an inline
    /// object that fully declares the source/sha/format in place. See
    /// `spec/binary-source.md` § "Bundle schema change".
    #[serde(default)]
    pub binaries: Vec<BundleBinary>,
    #[serde(default)]
    pub subcommands: Vec<String>,
}

impl Bundle {
    /// All binary names this bundle references, regardless of inline vs
    /// registry form. Used by callers that only need the namespace, not the
    /// full declaration (e.g., profile description rendering).
    pub fn binary_names(&self) -> impl Iterator<Item = &str> {
        self.binaries.iter().map(|b| b.name())
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct ProfileDef {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub bundles: Vec<String>,
    #[serde(default)]
    pub tools: Option<ToolsDef>,
    #[serde(default)]
    pub deny: Vec<String>,
    #[serde(default)]
    pub filesystem: Option<FilesystemDef>,
    #[serde(default)]
    pub network: Option<NetworkDef>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub credentials: BTreeMap<String, CredentialEntry>,
}

/// A credential entry: either a preset reference or a full provider definition.
#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum CredentialEntry {
    /// Preset reference: `gcloud: preset`
    Preset(String),
    /// Full provider definition: `{ provider: command, command: "...", inject: { ... } }`
    Custom(CredentialDef),
}

/// A credential provider definition in the config.
#[derive(Debug, Deserialize, Clone)]
pub struct CredentialDef {
    pub provider: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub env: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub inject: HashMap<String, String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ToolsDef {
    #[serde(default)]
    pub binaries: Vec<String>,
    #[serde(default)]
    pub subcommands: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FilesystemDef {
    pub workspace: Option<String>,
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub deny_read: Vec<String>,
    #[serde(default)]
    pub deny_write: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct NetworkDef {
    #[serde(default)]
    pub allow: Vec<String>,
}

/// A resolved profile ready for use by the sandbox engine.
#[derive(Debug)]
pub struct Profile {
    pub name: String,
    pub binaries: HashSet<String>,
    pub subcommand_allows: Vec<String>,
    pub subcommand_denies: Vec<String>,
    pub workspace: Option<PathBuf>,
    pub read_paths: Vec<PathBuf>,
    pub deny_read_paths: Vec<PathBuf>,
    pub deny_write_paths: Vec<PathBuf>,
    pub network_allow: Vec<String>,
    pub env: HashMap<String, String>,
    /// Resolved binary references — one per binary name in `binaries`.
    /// `Cached` entries describe a cache-managed binary (file path inside the
    /// cache root). `HostPath` falls back to today's `which`-based discovery.
    /// Filled by `resolve_profile_with_identity` against the top-level
    /// `binaries:` registry + inline bundle declarations + host PATH.
    pub resolved_binaries: Vec<ResolvedBinaryRef>,
}

impl OstiaConfig {
    /// Synchronous loader. Parses the bootstrap YAML at `path` into an
    /// `OstiaConfig`. Does NOT dispatch the `profile_source` block — when
    /// `profile_source` is present, the resulting `OstiaConfig` will have
    /// the parsed source-def attached but `bundles` and `profiles` will be
    /// whatever was inline in the file (which is typically empty in a
    /// production bootstrap).
    ///
    /// Use this for `ostia run` and `ostia check` (legacy dev-loop tools).
    /// For `ostia serve`, use `load_resolved` instead so the source's data
    /// actually populates `bundles` and `profiles`.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let config: OstiaConfig = serde_yaml::from_str(&contents)?;
        Ok(config)
    }

    /// Async loader that dispatches the configured `profile_source`. When the
    /// source returns successfully, its `bundles` and `profiles` REPLACE
    /// whatever was inline in the bootstrap config. `endpoints`, `auth`, and
    /// `profile_source` from the bootstrap are preserved.
    ///
    /// Returns an error if the source can't be reached or returns invalid
    /// content. Callers (typically `serve.rs:run_serve`) should let this
    /// error propagate — it indicates startup should fail loudly.
    ///
    /// Use [`OstiaConfig::load_resolved_with_cache`] when the caller needs
    /// the cached source handle for live refresh (Slice 2). This entry point
    /// is kept for callers that only need a one-shot load.
    pub async fn load_resolved(path: &Path) -> anyhow::Result<Self> {
        let (config, _) = Self::load_resolved_with_cache(path).await?;
        Ok(config)
    }

    /// Same as [`OstiaConfig::load_resolved`] but ALSO returns the cached
    /// source handle so callers can drive live refresh after startup. The
    /// returned cache is primed with the initial load's data so the first
    /// post-startup refresh check is a no-op until TTL expires.
    pub async fn load_resolved_with_cache(
        path: &Path,
    ) -> anyhow::Result<(Self, Option<std::sync::Arc<crate::source::CachedProfileSource>>)> {
        let mut config = Self::load(path)?;
        let cache = if let Some(source_def) = config.profile_source.clone() {
            let source = source_def.build()?;
            let sourced = source.load().await?;
            if !config.profiles.is_empty() || !config.bundles.is_empty() {
                eprintln!(
                    "warning: inline `bundles:` / `profiles:` in bootstrap config are ignored when `profile_source:` is set"
                );
            }
            config.bundles = sourced.bundles.clone();
            config.profiles = sourced.profiles.clone();

            let cached = crate::source::CachedProfileSource::new(source, source_def.cache_ttl());
            cached.prime(&sourced).await;
            Some(cached)
        } else {
            None
        };
        Ok((config, cache))
    }

    pub fn resolve_profile(&self, name: &str) -> anyhow::Result<Profile> {
        self.resolve_profile_with_identity(name, None)
    }

    pub fn resolve_profile_with_identity(&self, name: &str, user_id: Option<&str>) -> anyhow::Result<Profile> {
        let profile_def = self
            .profiles
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("profile '{}' not found in config", name))?;

        let mut binaries = HashSet::new();
        let mut subcommand_allows = Vec::new();
        // (name → entry) tracker for within-profile conflict detection. Each
        // binary name in this profile that resolves to a cache-managed entry
        // must agree on its sha across all bundles that reference it.
        let mut resolved_map: HashMap<String, ResolvedBinaryRef> = HashMap::new();
        let mut shas_seen: HashMap<String, String> = HashMap::new();

        // Merge bundles (config-defined take precedence over built-ins)
        let builtins = crate::builtins::builtin_bundles();
        for bundle_name in &profile_def.bundles {
            let bundle = self
                .bundles
                .get(bundle_name)
                .or_else(|| builtins.get(bundle_name))
                .ok_or_else(|| anyhow::anyhow!("bundle '{}' not found in config or built-ins", bundle_name))?;
            for bin in &bundle.binaries {
                let bin_name = bin.name().to_string();
                binaries.insert(bin_name.clone());
                let entry_for_resolution: Option<BinaryEntry> = match bin {
                    BundleBinary::Inline(inline) => Some(inline.to_entry()),
                    BundleBinary::Name(_) => self.binaries.get(&bin_name).cloned(),
                };
                let resolved = match entry_for_resolution {
                    Some(entry) => {
                        if let Some(prev_sha) = shas_seen.get(&bin_name) {
                            if prev_sha != &entry.sha256 {
                                anyhow::bail!(
                                    "profile `{}`: binary `{}` has inconsistent sha256 across bundles: \
                                     `{}` (conflict / mismatch). Pin a single version per profile.",
                                    name,
                                    bin_name,
                                    entry.sha256,
                                );
                            }
                        } else {
                            shas_seen.insert(bin_name.clone(), entry.sha256.clone());
                        }
                        ResolvedBinaryRef::Cached {
                            name: bin_name.clone(),
                            sha256: entry.sha256.clone(),
                            entry,
                        }
                    }
                    None => ResolvedBinaryRef::HostPath {
                        name: bin_name.clone(),
                    },
                };
                resolved_map.insert(bin_name, resolved);
            }
            subcommand_allows.extend(bundle.subcommands.iter().cloned());
        }

        // Add profile-level tools (still string form; resolve against registry)
        if let Some(tools) = &profile_def.tools {
            for tool_bin in &tools.binaries {
                binaries.insert(tool_bin.clone());
                if !resolved_map.contains_key(tool_bin) {
                    let resolved = match self.binaries.get(tool_bin) {
                        Some(entry) => {
                            shas_seen
                                .entry(tool_bin.clone())
                                .or_insert_with(|| entry.sha256.clone());
                            ResolvedBinaryRef::Cached {
                                name: tool_bin.clone(),
                                sha256: entry.sha256.clone(),
                                entry: entry.clone(),
                            }
                        }
                        None => ResolvedBinaryRef::HostPath {
                            name: tool_bin.clone(),
                        },
                    };
                    resolved_map.insert(tool_bin.clone(), resolved);
                }
            }
            subcommand_allows.extend(tools.subcommands.iter().cloned());
        }

        let (workspace, read_paths, deny_read_paths, deny_write_paths) =
            if let Some(fs) = &profile_def.filesystem {
                (
                    fs.workspace.as_ref().map(PathBuf::from),
                    fs.read.iter().map(PathBuf::from).collect(),
                    fs.deny_read.iter().map(PathBuf::from).collect(),
                    fs.deny_write.iter().map(PathBuf::from).collect(),
                )
            } else {
                (None, vec![], vec![], vec![])
            };

        let network_allow = profile_def
            .network
            .as_ref()
            .map(|n| n.allow.clone())
            .unwrap_or_default();

        // Resolve credential entries (presets → full definitions), then fetch.
        let mut env = profile_def.env.clone();
        if !profile_def.credentials.is_empty() {
            let resolved_creds = crate::credentials::resolve_entries(&profile_def.credentials)?;
            let cred_env = crate::credentials::fetch_credentials(&resolved_creds, user_id)?;
            env.extend(cred_env);
        }

        let mut resolved_binaries: Vec<ResolvedBinaryRef> =
            resolved_map.into_values().collect();
        // Stable ordering by binary name so downstream walkers behave the same
        // run-to-run (useful in tests + log output).
        resolved_binaries.sort_by(|a, b| a.name().cmp(b.name()));

        Ok(Profile {
            name: name.to_string(),
            binaries,
            subcommand_allows,
            subcommand_denies: profile_def.deny.clone(),
            workspace,
            read_paths,
            deny_read_paths,
            deny_write_paths,
            network_allow,
            env,
            resolved_binaries,
        })
    }

    /// Build a curated tool description for a profile.
    ///
    /// Includes: profile description, featured bundle tools, notable denials,
    /// and workspace path.
    pub fn build_tool_description(&self, name: &str, profile_def: &ProfileDef) -> String {
        let mut parts = Vec::new();

        // Opening line: profile description or name
        parts.push(
            profile_def
                .description
                .clone()
                .unwrap_or_else(|| name.to_string()),
        );

        // Featured tools: bundle descriptions (only bundles with description set)
        let builtins = crate::builtins::builtin_bundles();
        let featured: Vec<&str> = profile_def
            .bundles
            .iter()
            .filter_map(|bundle_name| {
                self.bundles
                    .get(bundle_name)
                    .or_else(|| builtins.get(bundle_name))
                    .and_then(|b| b.description.as_deref())
            })
            .collect();
        if !featured.is_empty() {
            parts.push(format!("Tools: {}", featured.join(", ")));
        }

        // Notable denials: deny patterns whose binary is in the profile
        let all_binaries: HashSet<String> = profile_def
            .bundles
            .iter()
            .filter_map(|bundle_name| {
                self.bundles
                    .get(bundle_name)
                    .or_else(|| builtins.get(bundle_name))
            })
            .flat_map(|b| b.binary_names().map(|n| n.to_string()))
            .collect();

        let notable: Vec<&str> = profile_def
            .deny
            .iter()
            .filter(|pattern| {
                let binary = pattern.split_whitespace().next().unwrap_or("");
                all_binaries.contains(binary)
            })
            .map(|s| s.as_str())
            .collect();
        if !notable.is_empty() {
            parts.push(format!("Denied: {}", notable.join(", ")));
        }

        // Workspace path
        if let Some(fs) = &profile_def.filesystem {
            if let Some(ws) = &fs.workspace {
                parts.push(format!("Workspace: {}", ws));
            }
        }

        parts.join("\n")
    }

    /// Resolve a profile from a token. In open mode (or no auth config), the
    /// token is the raw profile name. In token mode, it is an AES-GCM encrypted
    /// profile name that gets decrypted first.
    pub fn resolve_profile_from_token(&self, token: &str) -> anyhow::Result<Profile> {
        let profile_name = match &self.auth {
            Some(auth_cfg) if auth_cfg.mode == "token" => {
                let key = auth_cfg
                    .key
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("token mode requires auth.key in config"))?;
                decrypt_profile_token(key, token)?
            }
            _ => token.to_string(),
        };
        self.resolve_profile(&profile_name)
    }
}

fn decrypt_profile_token(key_b64: &str, token: &str) -> anyhow::Result<String> {
    use aes_gcm::{aead::Aead, aead::generic_array::GenericArray, Aes256Gcm, KeyInit};
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

    let key_bytes = BASE64
        .decode(key_b64)
        .map_err(|e| anyhow::anyhow!("invalid auth token: {}", e))?;
    let token_bytes = BASE64
        .decode(token)
        .map_err(|e| anyhow::anyhow!("invalid auth token: {}", e))?;

    if token_bytes.len() <= 12 {
        anyhow::bail!("invalid auth token: too short");
    }

    let (nonce_bytes, ciphertext) = token_bytes.split_at(12);
    let cipher = Aes256Gcm::new_from_slice(&key_bytes)
        .map_err(|_| anyhow::anyhow!("invalid auth key: must be 32 bytes"))?;
    let nonce = GenericArray::from_slice(nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| anyhow::anyhow!("invalid auth token: decryption failed"))?;

    String::from_utf8(plaintext)
        .map_err(|_| anyhow::anyhow!("invalid auth token: not valid UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_config() {
        let yaml = r#"
profiles:
  test:
    bundles: []
    tools:
      binaries: [echo]
"#;
        let config: OstiaConfig = serde_yaml::from_str(yaml).unwrap();
        let profile = config.resolve_profile("test").unwrap();
        assert!(profile.binaries.contains("echo"));
    }

    #[test]
    fn test_bundle_composition() {
        let yaml = r#"
bundles:
  baseline:
    binaries: [cat, ls]
  git:
    binaries: [git]
    subcommands:
      - git log *
      - git status

profiles:
  dev:
    bundles: [baseline, git]
    tools:
      binaries: [npm]
    deny:
      - git push *
"#;
        let config: OstiaConfig = serde_yaml::from_str(yaml).unwrap();
        let profile = config.resolve_profile("dev").unwrap();
        assert!(profile.binaries.contains("cat"));
        assert!(profile.binaries.contains("git"));
        assert!(profile.binaries.contains("npm"));
        assert_eq!(profile.subcommand_allows.len(), 2);
        assert_eq!(profile.subcommand_denies.len(), 1);
    }

    #[test]
    fn test_missing_profile_error() {
        let yaml = r#"
profiles:
  test:
    bundles: []
"#;
        let config: OstiaConfig = serde_yaml::from_str(yaml).unwrap();
        let result = config.resolve_profile("nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }
}
