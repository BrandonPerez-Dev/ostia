use serde::Deserialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

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
}

#[derive(Debug, Deserialize, Clone)]
pub struct Bundle {
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub binaries: Vec<String>,
    #[serde(default)]
    pub subcommands: Vec<String>,
}

#[derive(Debug, Deserialize)]
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
    pub auth: BTreeMap<String, AuthCheckDef>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AuthCheckDef {
    pub check: String,
}

#[derive(Debug, Deserialize)]
pub struct ToolsDef {
    #[serde(default)]
    pub binaries: Vec<String>,
    #[serde(default)]
    pub subcommands: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct FilesystemDef {
    pub workspace: Option<String>,
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub deny_read: Vec<String>,
    #[serde(default)]
    pub deny_write: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct NetworkDef {
    #[serde(default)]
    pub allow: Vec<String>,
}

/// An auth check to run on the host before sandbox execution.
#[derive(Debug, Clone)]
pub struct AuthCheck {
    pub service: String,
    pub command: String,
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
    pub auth_checks: Vec<AuthCheck>,
}

impl OstiaConfig {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let config: OstiaConfig = serde_yaml::from_str(&contents)?;
        Ok(config)
    }

    pub fn resolve_profile(&self, name: &str) -> anyhow::Result<Profile> {
        let profile_def = self
            .profiles
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("profile '{}' not found in config", name))?;

        let mut binaries = HashSet::new();
        let mut subcommand_allows = Vec::new();

        // Merge bundles (config-defined take precedence over built-ins)
        let builtins = crate::builtins::builtin_bundles();
        for bundle_name in &profile_def.bundles {
            let bundle = self
                .bundles
                .get(bundle_name)
                .or_else(|| builtins.get(bundle_name))
                .ok_or_else(|| anyhow::anyhow!("bundle '{}' not found in config or built-ins", bundle_name))?;
            binaries.extend(bundle.binaries.iter().cloned());
            subcommand_allows.extend(bundle.subcommands.iter().cloned());
        }

        // Add profile-level tools
        if let Some(tools) = &profile_def.tools {
            binaries.extend(tools.binaries.iter().cloned());
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

        let auth_checks = profile_def
            .auth
            .iter()
            .map(|(k, v)| AuthCheck {
                service: k.clone(),
                command: v.check.clone(),
            })
            .collect();

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
            auth_checks,
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
            .flat_map(|b| b.binaries.iter().cloned())
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
