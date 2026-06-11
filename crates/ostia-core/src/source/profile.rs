//! Profile-source trait, auth-source enum, and bootstrap-config types.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use secrecy::SecretString;
use serde::Deserialize;

use crate::config::{Bundle, ProfileDef};

/// Default cache TTL applied when a `cache_ttl:` field is absent from
/// `profile_source:`. See `spec/profile-source.md` Slice 2.
pub fn default_cache_ttl() -> Duration {
    Duration::from_secs(30)
}

/// Parsed bundles + profiles returned by a source's `load()` call.
///
/// This is the operational content the source provides. Bootstrap concerns
/// (server-level auth mode, endpoints) stay on the outer `OstiaConfig`.
#[derive(Debug, Default)]
pub struct SourcedConfig {
    pub bundles: HashMap<String, Bundle>,
    pub profiles: HashMap<String, ProfileDef>,
}

/// The async surface every profile-source implementation exposes.
///
/// Implementations live in sibling modules (`file`, `http`, `postgres`). The
/// trait is intentionally narrow — one method, returns parsed config or an
/// error. Refresh, hot registration, and per-profile lookup are deferred to
/// later slices.
#[async_trait]
pub trait ProfileSource: Send + Sync {
    async fn load(&self) -> anyhow::Result<SourcedConfig>;
}

// ─── Auth ─────────────────────────────────────────────────────────────

/// Auth declaration in the bootstrap YAML (`profile_source.auth`).
///
/// Discriminated on a `type` field. Slice 1 implements `none`, `static_secret`,
/// and `tls_identity`. `dynamic_token` is reserved for OAuth2 / IAM tokens in
/// later slices — using it in Slice 1 is a config error.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthSourceDef {
    /// No auth (peer-trusted unix socket, presigned URL, public endpoint).
    None,
    /// A static secret resolved from an environment variable. The variable
    /// name is provider-specific: `password_env` for postgres, `bearer_env`
    /// for http. Exactly one of those fields must be set for the given provider.
    StaticSecret {
        #[serde(default)]
        password_env: Option<String>,
        #[serde(default)]
        bearer_env: Option<String>,
    },
    /// Dynamically-refreshed token (OAuth2 client creds, AWS IAM, etc.).
    /// Reserved — not implemented in Slice 1.
    DynamicToken,
    /// Client certificate + key for mTLS.
    TlsIdentity {
        cert_path: String,
        key_path: String,
    },
}

impl Default for AuthSourceDef {
    fn default() -> Self {
        AuthSourceDef::None
    }
}

/// Resolved auth value, ready to hand to a protocol-specific surface.
///
/// `StaticSecret` wraps a `SecretString` for zeroize-on-drop hygiene. Debug
/// prints `[REDACTED]` so logs don't leak the value if a provider fails.
#[derive(Debug, Clone)]
pub enum AuthSource {
    None,
    StaticSecret(SecretString),
    TlsIdentity {
        cert_path: String,
        key_path: String,
    },
}

impl AuthSource {
    /// Resolve an `AuthSourceDef` into a runtime `AuthSource`. Env vars are
    /// looked up at this point; missing vars produce a clear error before
    /// any network call.
    ///
    /// `which_env` is the variant-specific accessor — `password_env` for
    /// postgres providers, `bearer_env` for http providers. The choice is
    /// the caller's because each provider knows which field is meaningful.
    pub fn resolve(def: &AuthSourceDef, which_env: WhichEnv) -> anyhow::Result<Self> {
        match def {
            AuthSourceDef::None => Ok(AuthSource::None),
            AuthSourceDef::StaticSecret {
                password_env,
                bearer_env,
            } => {
                let var = match which_env {
                    WhichEnv::Password => password_env.as_ref(),
                    WhichEnv::Bearer => bearer_env.as_ref(),
                };
                let Some(name) = var else {
                    anyhow::bail!(
                        "profile source auth: static_secret requires `{}` to name an env var",
                        match which_env {
                            WhichEnv::Password => "password_env",
                            WhichEnv::Bearer => "bearer_env",
                        }
                    );
                };
                let value = std::env::var(name).map_err(|_| {
                    anyhow::anyhow!(
                        "profile source auth: env var `{}` is not set (referenced by static_secret)",
                        name
                    )
                })?;
                Ok(AuthSource::StaticSecret(SecretString::from(value)))
            }
            AuthSourceDef::DynamicToken => anyhow::bail!(
                "profile source auth: `dynamic_token` is reserved for future slices and not supported in Slice 1"
            ),
            AuthSourceDef::TlsIdentity {
                cert_path,
                key_path,
            } => Ok(AuthSource::TlsIdentity {
                cert_path: cert_path.clone(),
                key_path: key_path.clone(),
            }),
        }
    }
}

/// Which env-var-naming field on `AuthSourceDef::StaticSecret` a provider cares
/// about. Postgres uses `password_env`; HTTP uses `bearer_env`.
#[derive(Debug, Clone, Copy)]
pub enum WhichEnv {
    Password,
    Bearer,
}

// ─── Provider discriminator ───────────────────────────────────────────

/// The `profile_source:` block at the top of the bootstrap YAML.
///
/// Tagged on `provider` — an unknown value (e.g., `provider: ftp`) is a clean
/// serde deserialization error mentioning the bad variant.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "provider", rename_all = "lowercase")]
pub enum ProfileSourceDef {
    File(FileSourceDef),
    Http(HttpSourceDef),
    Postgres(PostgresSourceDef),
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileSourceDef {
    pub path: String,
    #[serde(default)]
    pub auth: AuthSourceDef,
    #[serde(default = "default_cache_ttl", with = "humantime_serde")]
    pub cache_ttl: Duration,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HttpSourceDef {
    pub url: String,
    #[serde(default)]
    pub auth: AuthSourceDef,
    #[serde(default = "default_cache_ttl", with = "humantime_serde")]
    pub cache_ttl: Duration,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PostgresSourceDef {
    pub dsn: String,
    #[serde(default)]
    pub auth: AuthSourceDef,
    #[serde(default = "default_cache_ttl", with = "humantime_serde")]
    pub cache_ttl: Duration,
}

impl ProfileSourceDef {
    /// Construct the concrete async provider for this definition.
    pub fn build(&self) -> anyhow::Result<Box<dyn ProfileSource>> {
        match self {
            ProfileSourceDef::File(def) => Ok(Box::new(super::file::FileProfileSource::new(
                def.path.clone(),
            ))),
            ProfileSourceDef::Http(def) => {
                Ok(Box::new(super::http::HttpProfileSource::new(def.clone())?))
            }
            ProfileSourceDef::Postgres(def) => Ok(Box::new(
                super::postgres::PostgresProfileSource::new(def.clone())?,
            )),
        }
    }

    /// TTL for the in-process cache that backs this source. Set per-source in
    /// the bootstrap config; defaults to 30s when unset.
    pub fn cache_ttl(&self) -> Duration {
        match self {
            ProfileSourceDef::File(d) => d.cache_ttl,
            ProfileSourceDef::Http(d) => d.cache_ttl,
            ProfileSourceDef::Postgres(d) => d.cache_ttl,
        }
    }
}
