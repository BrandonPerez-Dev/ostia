//! Integration tests: binary-source postgres-blob provider (Slice 3 of #11).
//! Covers C-BS8 from `spec/binary-source.md`.

mod mcp_common;

use serde_json::json;
use std::io::Write;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres as PostgresImage;

fn assert_docker_available() {
    let output = std::process::Command::new("docker").arg("info").output();
    let ok = matches!(output, Ok(o) if o.status.success());
    assert!(
        ok,
        "docker required for binary_source_postgres tests"
    );
}

async fn seed_pg_with_binary_blob(
    dsn: &str,
    workspace: &str,
    binary_bytes: &[u8],
    binary_sha: &str,
) {
    let (client, connection) = tokio_postgres::connect(dsn, tokio_postgres::NoTls)
        .await
        .expect("connect to test postgres");
    let handle = tokio::spawn(async move {
        let _ = connection.await;
    });

    client
        .batch_execute(
            r#"
            CREATE TABLE IF NOT EXISTS bundles (
                name TEXT PRIMARY KEY,
                definition JSONB NOT NULL,
                updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
            );
            CREATE TABLE IF NOT EXISTS profiles (
                name TEXT PRIMARY KEY,
                definition JSONB NOT NULL,
                updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
            );
            CREATE TABLE IF NOT EXISTS binaries (
                name TEXT PRIMARY KEY,
                definition JSONB NOT NULL,
                updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
            );
            CREATE TABLE IF NOT EXISTS binaries_blobs (
                name TEXT PRIMARY KEY,
                bytes BYTEA NOT NULL,
                sha256 TEXT NOT NULL
            );
            "#,
        )
        .await
        .expect("create schema");

    let baseline = json!({ "binaries": ["sh", "bash"] });
    client
        .execute(
            "INSERT INTO bundles (name, definition) VALUES ($1, $2)",
            &[&"baseline", &baseline],
        )
        .await
        .expect("insert baseline bundle");

    // Bundle that references `jq` by name from registry
    let jq_bundle = json!({ "binaries": ["sh", "bash", "jq"] });
    client
        .execute(
            "INSERT INTO bundles (name, definition) VALUES ($1, $2)",
            &[&"with-jq", &jq_bundle],
        )
        .await
        .expect("insert with-jq bundle");

    let profile_def = json!({
        "bundles": ["with-jq"],
        "filesystem": { "workspace": workspace }
    });
    client
        .execute(
            "INSERT INTO profiles (name, definition) VALUES ($1, $2)",
            &[&"test", &profile_def],
        )
        .await
        .expect("insert profile");

    // Binary registry — jq lives in the postgres-blob source
    let binary_def = json!({
        "sha256": binary_sha,
        "format": "binary",
        "source": {
            "provider": "postgres-blob",
            "table": "binaries_blobs",
            "key_column": "name",
            "value_column": "bytes",
            "key": "jq-test"
        }
    });
    client
        .execute(
            "INSERT INTO binaries (name, definition) VALUES ($1, $2)",
            &[&"jq", &binary_def],
        )
        .await
        .expect("insert binary registry");

    // Blob with the bytes
    client
        .execute(
            "INSERT INTO binaries_blobs (name, bytes, sha256) VALUES ($1, $2, $3)",
            &[&"jq-test", &binary_bytes, &binary_sha],
        )
        .await
        .expect("insert blob");

    drop(client);
    handle.abort();
}

async fn start_seeded_postgres_with_binary(
    workspace: &str,
    binary_bytes: &[u8],
    binary_sha: &str,
) -> (ContainerAsync<PostgresImage>, String) {
    let container = PostgresImage::default()
        .start()
        .await
        .expect("start postgres");
    let host = container.get_host().await.expect("host").to_string();
    let port = container.get_host_port_ipv4(5432).await.expect("port");
    let full_dsn = format!("postgres://postgres:postgres@{host}:{port}/postgres");
    seed_pg_with_binary_blob(&full_dsn, workspace, binary_bytes, binary_sha).await;
    let dsn = format!("postgres://postgres@{host}:{port}/postgres");
    (container, dsn)
}

// ─── C-BS8 ───

/// C-BS8: postgres-blob binary source — BYTEA column → binary in sandbox.
#[tokio::test]
async fn postgres_blob_source_lands_in_sandbox() {
    mcp_common::assert_user_namespaces();
    assert_docker_available();
    let workspace = tempfile::tempdir().expect("workspace");
    let cache_dir = mcp_common::temp_binary_cache_dir();

    let bytes = mcp_common::make_fake_binary_bytes("jq-from-blob");
    let sha = mcp_common::sha256_hex(&bytes);
    let (_container, dsn) =
        start_seeded_postgres_with_binary(workspace.path().to_str().unwrap(), &bytes, &sha).await;

    let config = format!(
        r#"binary_cache_dir: {cache_dir}

profile_source:
  provider: postgres
  dsn: "{dsn}"
  cache_ttl: 30s
  auth:
    type: static_secret
    password_env: OSTIA_TEST_PG_PASSWORD
"#,
        cache_dir = cache_dir.path().display(),
    );
    let mut config_file = tempfile::NamedTempFile::new().expect("config temp");
    config_file.write_all(config.as_bytes()).expect("write config");

    let mut client = mcp_common::McpClient::spawn_with_args_and_env(
        config_file.path(),
        &[],
        &[("OSTIA_TEST_PG_PASSWORD", "postgres")],
    );
    client.handshake();

    let response = client.call_tool("test", json!({ "command": "jq" }));

    let result = &response["result"];
    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("jq-from-blob"),
        "postgres-blob binary should run, got: {:?}",
        text
    );

    let cache_path = cache_dir.path().join(&sha).join("jq");
    assert!(
        cache_path.exists(),
        "postgres-blob binary should be cached at {}",
        cache_path.display()
    );
}
