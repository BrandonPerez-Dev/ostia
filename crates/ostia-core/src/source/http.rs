//! HTTP-backed profile source.
//!
//! Issues `GET <url>` with optional `Authorization: Bearer <env>` from
//! `auth.bearer_env`. Body content-type discriminates parsing:
//!   - `application/json` → JSON parse
//!   - anything else (including `application/yaml` and missing header) → YAML
//!
//! Non-2xx responses, missing env vars, and unparseable bodies all surface as
//! `Err` so `OstiaConfig::load_resolved` can propagate to startup failure.

use async_trait::async_trait;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use secrecy::ExposeSecret;

use super::file::SourceContent;
use super::profile::{
    AuthSource, AuthSourceDef, HttpSourceDef, ProfileSource, SourcedConfig, WhichEnv,
};

pub struct HttpProfileSource {
    def: HttpSourceDef,
}

impl HttpProfileSource {
    pub fn new(def: HttpSourceDef) -> anyhow::Result<Self> {
        // Validate auth resolves NOW (env vars present, no dynamic_token) so
        // startup fails before any network call if the bootstrap is wrong.
        // mTLS is allowed but TLS wiring is deferred — error if requested in
        // Slice 1.
        match &def.auth {
            AuthSourceDef::None => {}
            AuthSourceDef::StaticSecret { .. } => {
                let _ = AuthSource::resolve(&def.auth, WhichEnv::Bearer)?;
            }
            AuthSourceDef::TlsIdentity { .. } => {
                anyhow::bail!(
                    "profile source (http): tls_identity auth is reserved for later slices and not supported in Slice 1"
                );
            }
            AuthSourceDef::DynamicToken => {
                anyhow::bail!(
                    "profile source (http): dynamic_token auth is reserved for later slices"
                );
            }
        }
        Ok(Self { def })
    }

    fn build_headers(&self) -> anyhow::Result<HeaderMap> {
        let mut headers = HeaderMap::new();
        if let AuthSourceDef::StaticSecret { .. } = self.def.auth {
            let resolved = AuthSource::resolve(&self.def.auth, WhichEnv::Bearer)?;
            if let AuthSource::StaticSecret(secret) = resolved {
                let value = format!("Bearer {}", secret.expose_secret());
                let header = HeaderValue::from_str(&value).map_err(|e| {
                    anyhow::anyhow!(
                        "profile source (http): bearer token has invalid header characters: {}",
                        e
                    )
                })?;
                headers.insert(AUTHORIZATION, header);
            }
        }
        Ok(headers)
    }
}

#[async_trait]
impl ProfileSource for HttpProfileSource {
    async fn load(&self) -> anyhow::Result<SourcedConfig> {
        let client = reqwest::Client::builder().build().map_err(|e| {
            anyhow::anyhow!("profile source (http): failed to build HTTP client: {}", e)
        })?;

        let headers = self.build_headers()?;
        let response = client
            .get(&self.def.url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "profile source (http): request to `{}` failed: {}",
                    self.def.url,
                    e
                )
            })?;

        let status = response.status();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        if !status.is_success() {
            anyhow::bail!(
                "profile source (http): GET `{}` returned HTTP {} (server error / unsuccessful response)",
                self.def.url,
                status.as_u16()
            );
        }

        let body = response.text().await.map_err(|e| {
            anyhow::anyhow!(
                "profile source (http): failed to read body from `{}`: {}",
                self.def.url,
                e
            )
        })?;

        let parsed: SourceContent = if content_type
            .as_deref()
            .map(|ct| ct.to_lowercase().contains("application/json"))
            .unwrap_or(false)
        {
            serde_json::from_str(&body).map_err(|e| {
                anyhow::anyhow!(
                    "profile source (http): failed to parse JSON body from `{}`: {}",
                    self.def.url,
                    e
                )
            })?
        } else {
            serde_yaml::from_str(&body).map_err(|e| {
                anyhow::anyhow!(
                    "profile source (http): failed to parse YAML body from `{}`: {}",
                    self.def.url,
                    e
                )
            })?
        };

        Ok(parsed.into())
    }
}
