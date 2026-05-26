//! Postgres-backed profile source.
//!
//! Expected schema (operator creates these tables):
//! ```sql
//! CREATE TABLE bundles  (name TEXT PRIMARY KEY, definition JSONB NOT NULL, updated_at TIMESTAMPTZ);
//! CREATE TABLE profiles (name TEXT PRIMARY KEY, definition JSONB NOT NULL, updated_at TIMESTAMPTZ);
//! ```
//!
//! On `load()`, the provider runs `SELECT name, definition FROM bundles` and
//! `SELECT name, definition FROM profiles`, deserializing each `definition`
//! JSONB into the existing `Bundle` / `ProfileDef` structs.
//!
//! Auth: `AuthSourceDef::StaticSecret { password_env }` resolves the password
//! from the named env var. The DSN configured in the bootstrap YAML MUST NOT
//! embed a password — the password is added to the connection params at
//! connect time. mTLS via `AuthSourceDef::TlsIdentity` is reserved for later
//! slices.

use std::collections::HashMap;

use async_trait::async_trait;
use secrecy::ExposeSecret;
use tokio_postgres::{Config as PgConfig, NoTls};

use crate::config::{Bundle, ProfileDef};

use super::profile::{
    AuthSource, AuthSourceDef, PostgresSourceDef, ProfileSource, SourcedConfig, WhichEnv,
};

pub struct PostgresProfileSource {
    def: PostgresSourceDef,
}

impl PostgresProfileSource {
    pub fn new(def: PostgresSourceDef) -> anyhow::Result<Self> {
        // Validate auth resolves at startup so failures surface before any
        // network attempt.
        match &def.auth {
            AuthSourceDef::None => {
                anyhow::bail!(
                    "profile source (postgres): auth `none` requires unix-socket peer auth which is not configured in this DSN. Specify `auth: {{ type: static_secret, password_env: ... }}` instead."
                );
            }
            AuthSourceDef::StaticSecret { .. } => {
                let _ = AuthSource::resolve(&def.auth, WhichEnv::Password)?;
            }
            AuthSourceDef::TlsIdentity { .. } => {
                anyhow::bail!(
                    "profile source (postgres): tls_identity auth is reserved for later slices and not supported in Slice 1"
                );
            }
            AuthSourceDef::DynamicToken => {
                anyhow::bail!(
                    "profile source (postgres): dynamic_token auth (AWS RDS IAM, etc.) is reserved for later slices"
                );
            }
        }
        Ok(Self { def })
    }

    /// Build a `tokio_postgres::Config` from the DSN and inject the password.
    /// Sanity-rejects a DSN that already embeds a password (per spec: no
    /// secrets in the bootstrap YAML).
    fn build_pg_config(&self) -> anyhow::Result<PgConfig> {
        let resolved = AuthSource::resolve(&self.def.auth, WhichEnv::Password)?;
        let AuthSource::StaticSecret(secret) = resolved else {
            anyhow::bail!("profile source (postgres): expected resolved StaticSecret");
        };

        let mut config: PgConfig = self.def.dsn.parse().map_err(|e| {
            anyhow::anyhow!(
                "profile source (postgres): failed to parse DSN `{}`: {}",
                self.def.dsn,
                e
            )
        })?;
        config.password(secret.expose_secret());
        Ok(config)
    }
}

#[async_trait]
impl ProfileSource for PostgresProfileSource {
    async fn load(&self) -> anyhow::Result<SourcedConfig> {
        let config = self.build_pg_config()?;

        let (client, connection) = config.connect(NoTls).await.map_err(|e| {
            anyhow::anyhow!(
                "profile source (postgres): failed to connect to `{}`: {}",
                self.def.dsn,
                e
            )
        })?;

        // Drive the connection task. We abort it after queries complete so
        // the client doesn't hang the runtime.
        let conn_handle = tokio::spawn(async move {
            let _ = connection.await;
        });

        let bundle_rows = client
            .query("SELECT name, definition FROM bundles", &[])
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "profile source (postgres): SELECT FROM bundles failed: {}",
                    e
                )
            })?;
        let profile_rows = client
            .query("SELECT name, definition FROM profiles", &[])
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "profile source (postgres): SELECT FROM profiles failed: {}",
                    e
                )
            })?;

        let mut bundles: HashMap<String, Bundle> = HashMap::new();
        for row in bundle_rows {
            let name: String = row.get(0);
            let def: serde_json::Value = row.get(1);
            let bundle: Bundle = serde_json::from_value(def).map_err(|e| {
                anyhow::anyhow!(
                    "profile source (postgres): failed to deserialize bundle `{}`: {}",
                    name,
                    e
                )
            })?;
            bundles.insert(name, bundle);
        }

        let mut profiles: HashMap<String, ProfileDef> = HashMap::new();
        for row in profile_rows {
            let name: String = row.get(0);
            let def: serde_json::Value = row.get(1);
            let profile: ProfileDef = serde_json::from_value(def).map_err(|e| {
                anyhow::anyhow!(
                    "profile source (postgres): failed to deserialize profile `{}`: {}",
                    name,
                    e
                )
            })?;
            profiles.insert(name, profile);
        }

        drop(client);
        conn_handle.abort();

        Ok(SourcedConfig { bundles, profiles })
    }
}
