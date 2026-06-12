//! Binary source fetch implementations.
//!
//! Three providers in Slice 3: `file` (read local), `http` (rustls GET), and
//! `postgres-blob` (SELECT BYTEA from a column on the ambient postgres
//! connection from the `profile_source`). Each translates a `BinarySourceDef`
//! variant into raw bytes ready for `BinaryCache::stage`.

use std::path::Path;

use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use secrecy::ExposeSecret;

use crate::source::{AuthSource, AuthSourceDef, ProfileSourceDef, WhichEnv};

use super::schema::{BinarySourceDef, HttpAuthDef};

#[derive(Debug, thiserror::Error)]
pub enum BinaryFetchError {
    #[error("binary source ({provider}): {message}")]
    Provider {
        provider: &'static str,
        message: String,
    },
}

/// Connection parameters carried from the ambient `profile_source` to the
/// `postgres-blob` binary source. Operators don't repeat the DSN/auth per
/// binary — Slice 3 inherits them from the profile-source connection.
#[derive(Debug, Clone)]
pub struct PostgresBlobParams {
    pub dsn: String,
    pub auth: AuthSourceDef,
}

impl PostgresBlobParams {
    /// Build from a `ProfileSourceDef`. Errors clearly if the profile source
    /// isn't postgres — `postgres-blob` binary sources require an ambient
    /// postgres profile_source connection.
    pub fn from_profile_source(def: &ProfileSourceDef) -> anyhow::Result<Self> {
        match def {
            ProfileSourceDef::Postgres(pg) => Ok(Self {
                dsn: pg.dsn.clone(),
                auth: pg.auth.clone(),
            }),
            _ => anyhow::bail!(
                "binary source (postgres-blob) requires a postgres `profile_source` to reuse its connection; \
                 the current profile_source is not postgres"
            ),
        }
    }
}

/// Fetch the raw bytes for the binary at `source`. Network-bound providers
/// run on the tokio runtime; the file provider runs sync internally but
/// fits this async surface.
///
/// `postgres_blob_params` is consulted ONLY when `source` is
/// `BinarySourceDef::PostgresBlob`. Passing `None` plus a postgres-blob source
/// is a clear error.
pub async fn fetch_bytes(
    name: &str,
    source: &BinarySourceDef,
    postgres_blob_params: Option<&PostgresBlobParams>,
) -> anyhow::Result<Vec<u8>> {
    match source {
        BinarySourceDef::File { path } => fetch_file(name, path).await,
        BinarySourceDef::Http { url, auth } => fetch_http(name, url, auth).await,
        BinarySourceDef::PostgresBlob {
            table,
            key_column,
            value_column,
            key,
        } => {
            let params = postgres_blob_params.ok_or_else(|| {
                anyhow::anyhow!(
                    "binary source (postgres-blob) for `{}`: no ambient postgres profile_source — \
                     declare a `profile_source: postgres ...` to use postgres-blob binaries",
                    name
                )
            })?;
            fetch_postgres_blob(name, params, table, key_column, value_column, key).await
        }
    }
}

async fn fetch_file(name: &str, path: &str) -> anyhow::Result<Vec<u8>> {
    let p = Path::new(path);
    let bytes = std::fs::read(p).map_err(|e| {
        anyhow::anyhow!(
            "binary source (file) for `{}`: failed to read `{}`: {}",
            name,
            path,
            e
        )
    })?;
    Ok(bytes)
}

async fn fetch_http(name: &str, url: &str, auth: &HttpAuthDef) -> anyhow::Result<Vec<u8>> {
    let client = reqwest::Client::builder().build().map_err(|e| {
        anyhow::anyhow!(
            "binary source (http) for `{}`: failed to build HTTP client: {}",
            name,
            e
        )
    })?;

    let mut headers = HeaderMap::new();
    match auth {
        HttpAuthDef::Absent => {}
        HttpAuthDef::Inherited(def) => match def {
            AuthSourceDef::None => {}
            AuthSourceDef::StaticSecret { .. } => {
                let resolved = AuthSource::resolve(def, WhichEnv::Bearer).map_err(|e| {
                    anyhow::anyhow!(
                        "binary source (http) for `{}`: auth resolution failed: {}",
                        name,
                        e
                    )
                })?;
                if let AuthSource::StaticSecret(secret) = resolved {
                    let value = format!("Bearer {}", secret.expose_secret());
                    let header = HeaderValue::from_str(&value).map_err(|e| {
                        anyhow::anyhow!(
                            "binary source (http) for `{}`: bearer token has invalid header characters: {}",
                            name,
                            e
                        )
                    })?;
                    headers.insert(AUTHORIZATION, header);
                }
            }
            AuthSourceDef::DynamicToken => anyhow::bail!(
                "binary source (http) for `{}`: dynamic_token auth is reserved for later slices",
                name
            ),
            AuthSourceDef::TlsIdentity { .. } => anyhow::bail!(
                "binary source (http) for `{}`: tls_identity auth is not supported in Slice 3",
                name
            ),
        },
    }

    let response = client
        .get(url)
        .headers(headers)
        .send()
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "binary source (http) for `{}`: GET `{}` failed: {}",
                name,
                url,
                e
            )
        })?;

    let status = response.status();
    if !status.is_success() {
        anyhow::bail!(
            "binary source (http) for `{}`: GET `{}` returned HTTP {}",
            name,
            url,
            status.as_u16()
        );
    }

    let bytes = response.bytes().await.map_err(|e| {
        anyhow::anyhow!(
            "binary source (http) for `{}`: failed to read response body from `{}`: {}",
            name,
            url,
            e
        )
    })?;
    Ok(bytes.to_vec())
}

async fn fetch_postgres_blob(
    name: &str,
    params: &PostgresBlobParams,
    table: &str,
    key_column: &str,
    value_column: &str,
    key: &str,
) -> anyhow::Result<Vec<u8>> {
    use tokio_postgres::{Config as PgConfig, NoTls};

    // Resolve the postgres password from the ambient profile-source auth.
    let resolved_auth = AuthSource::resolve(&params.auth, WhichEnv::Password).map_err(|e| {
        anyhow::anyhow!(
            "binary source (postgres-blob) for `{}`: auth resolution failed: {}",
            name,
            e
        )
    })?;

    let mut pg_config: PgConfig = params.dsn.parse().map_err(|e| {
        anyhow::anyhow!(
            "binary source (postgres-blob) for `{}`: failed to parse DSN `{}`: {}",
            name,
            params.dsn,
            e
        )
    })?;
    if let AuthSource::StaticSecret(secret) = resolved_auth {
        pg_config.password(secret.expose_secret());
    }

    let (client, connection) = pg_config.connect(NoTls).await.map_err(|e| {
        anyhow::anyhow!(
            "binary source (postgres-blob) for `{}`: connect failed: {}",
            name,
            e
        )
    })?;
    let conn_handle = tokio::spawn(async move {
        let _ = connection.await;
    });

    // Identifiers can't be parameterized in postgres; sanitize against
    // anything that's not a valid SQL identifier character.
    if !is_sql_identifier(table) || !is_sql_identifier(key_column) || !is_sql_identifier(value_column) {
        anyhow::bail!(
            "binary source (postgres-blob) for `{}`: invalid table or column identifier",
            name
        );
    }

    let query = format!(
        "SELECT {value_column} FROM {table} WHERE {key_column} = $1",
        value_column = value_column,
        table = table,
        key_column = key_column,
    );

    let row = client
        .query_opt(query.as_str(), &[&key])
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "binary source (postgres-blob) for `{}`: query failed: {}",
                name,
                e
            )
        })?;

    drop(client);
    conn_handle.abort();

    let row = row.ok_or_else(|| {
        anyhow::anyhow!(
            "binary source (postgres-blob) for `{}`: no row in `{}` with `{}` = `{}`",
            name,
            table,
            key_column,
            key
        )
    })?;
    let bytes: Vec<u8> = row.get(0);
    Ok(bytes)
}

fn is_sql_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}
