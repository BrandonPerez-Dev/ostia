//! Integration tests: profile-source `postgres` provider (Slice 1 of #11).
//!
//! Covers contracts C-PS9, C-PS10, C-PS11, C-PS12, C-PS15 from
//! `spec/profile-source.md`. Tests use `testcontainers-rs` to spin up an
//! ephemeral Postgres container, apply the schema, insert fixtures, then
//! point `ostia serve` at the container's host:port via a bootstrap config.
//!
//! Postgres test infrastructure: per-test ephemeral container. Docker is
//! required — tests panic with a loud message if Docker isn't available,
//! matching the convention from `docker.rs` and `mcp_common::assert_user_namespaces`.
//!
//! Tests use `#[tokio::test]` so the runtime stays alive across the
//! container's async drop. `McpClient::spawn` is sync but does not block
//! the runtime.

mod mcp_common;

use serde_json::json;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres as PostgresImage;

// ─── Common test infrastructure ───

/// Verify Docker is available; panic loudly if not. Mirrors
/// `mcp_common::assert_user_namespaces` — no silent skipping.
fn assert_docker_available() {
    let output = std::process::Command::new("docker").arg("info").output();
    let ok = matches!(output, Ok(o) if o.status.success());
    assert!(
        ok,
        "docker is required for profile_source_postgres tests but is not available. \
         install docker and ensure the current user can run `docker info`, then re-run \
         `cargo test -p ostia-cli --test profile_source_postgres`."
    );
}

/// Apply the Slice 1 schema and insert a single `baseline` bundle + `test`
/// profile. Returns nothing — panics on any error.
async fn seed_database(dsn: &str, workspace: &str) {
    let (client, connection) = tokio_postgres::connect(dsn, tokio_postgres::NoTls)
        .await
        .expect("connect to test postgres");
    let connection_handle = tokio::spawn(async move {
        let _ = connection.await;
    });

    client
        .batch_execute(
            r#"
            CREATE TABLE IF NOT EXISTS bundles (
                name        TEXT PRIMARY KEY,
                definition  JSONB NOT NULL,
                updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
            );
            CREATE TABLE IF NOT EXISTS profiles (
                name        TEXT PRIMARY KEY,
                definition  JSONB NOT NULL,
                updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
            );
            "#,
        )
        .await
        .expect("create schema");

    let baseline = json!({ "binaries": ["sh", "bash", "echo", "cat", "ls"] });
    let test_profile = json!({
        "bundles": ["baseline"],
        "filesystem": { "workspace": workspace }
    });

    client
        .execute(
            "INSERT INTO bundles (name, definition) VALUES ($1, $2)",
            &[&"baseline", &baseline],
        )
        .await
        .expect("insert baseline bundle");

    client
        .execute(
            "INSERT INTO profiles (name, definition) VALUES ($1, $2)",
            &[&"test", &test_profile],
        )
        .await
        .expect("insert test profile");

    drop(client);
    connection_handle.abort();
}

/// Spin up a postgres testcontainer and seed it with bundles + profiles.
/// Returns `(container, dsn_without_password)`. Caller must keep the
/// container alive until tests done — and must call this from within a
/// tokio runtime context so the container's async drop has a runtime.
async fn start_seeded_postgres(workspace: &str) -> (ContainerAsync<PostgresImage>, String) {
    let container = PostgresImage::default()
        .start()
        .await
        .expect("start postgres testcontainer");
    let host = container
        .get_host()
        .await
        .expect("get container host")
        .to_string();
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("get container port");

    // testcontainers-modules postgres default: user `postgres`, password `postgres`, db `postgres`
    let full_dsn = format!("postgres://postgres:postgres@{host}:{port}/postgres");
    seed_database(&full_dsn, workspace).await;

    // Return the DSN WITHOUT the password — bootstrap config will use this
    let dsn_without_password = format!("postgres://postgres@{host}:{port}/postgres");
    (container, dsn_without_password)
}

/// Find a free port without listening — for the connection-refused test.
fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind free-port probe");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

// ─── C-PS9: postgres happy path ───

/// C-PS9: Ostia connects to Postgres, queries the bundles + profiles tables,
/// and the loaded profile is reachable end-to-end via tools/call.
#[tokio::test]
async fn postgres_provider_loads_bundles_and_profiles() {
    // Arrange
    mcp_common::assert_user_namespaces();
    assert_docker_available();
    let workspace = tempfile::tempdir().expect("create workspace");
    let ws_path = workspace.path().to_str().unwrap().to_string();

    let (_container, dsn) = start_seeded_postgres(&ws_path).await;

    let bootstrap = mcp_common::write_bootstrap_with_profile_source(&format!(
        r#"  provider: postgres
  dsn: "{dsn}"
  auth:
    type: static_secret
    password_env: OSTIA_TEST_PG_PASSWORD"#
    ));

    let mut client = mcp_common::McpClient::spawn_with_args_and_env(
        bootstrap.path(),
        &[],
        &[("OSTIA_TEST_PG_PASSWORD", "postgres")],
    );
    client.handshake();

    // Act
    let tools = client.tools_list();
    let response = client.call_tool("test", json!({ "command": "echo postgres-loaded" }));

    // Assert
    let tools_array = tools["result"]["tools"]
        .as_array()
        .expect("tools/list returns tools array");
    assert!(
        tools_array.iter().any(|t| t["name"].as_str() == Some("test")),
        "profile `test` from postgres should appear in tools/list, got: {:?}",
        tools_array
    );

    let result = &response["result"];
    let is_error = result["isError"].as_bool().unwrap_or(false);
    assert!(!is_error, "tools/call should not be isError, got: {:?}", result);
    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("postgres-loaded"),
        "tools/call output should contain `postgres-loaded`, got: {:?}",
        text
    );
}

// ─── C-PS10: postgres password resolves from env var (not embedded in DSN) ───

/// C-PS10: When `auth.password_env` is set, the password is fetched from
/// the env var. The DSN in the bootstrap config does NOT contain `password=`.
#[tokio::test]
async fn postgres_provider_dsn_password_resolves_from_env() {
    // Arrange
    mcp_common::assert_user_namespaces();
    assert_docker_available();
    let workspace = tempfile::tempdir().expect("create workspace");
    let ws_path = workspace.path().to_str().unwrap().to_string();

    let (_container, dsn) = start_seeded_postgres(&ws_path).await;

    // Sanity check: DSN must NOT contain a password
    assert!(
        !dsn.to_lowercase().contains("password="),
        "test setup invariant: DSN must not embed password, got: {:?}",
        dsn
    );
    assert!(
        !dsn.contains("postgres:postgres@"),
        "test setup invariant: DSN must not embed userinfo password, got: {:?}",
        dsn
    );

    let bootstrap = mcp_common::write_bootstrap_with_profile_source(&format!(
        r#"  provider: postgres
  dsn: "{dsn}"
  auth:
    type: static_secret
    password_env: OSTIA_TEST_PG_PASSWORD"#
    ));

    // Act — spawn with the password ONLY in the env var
    let mut client = mcp_common::McpClient::spawn_with_args_and_env(
        bootstrap.path(),
        &[],
        &[("OSTIA_TEST_PG_PASSWORD", "postgres")],
    );
    client.handshake();
    let tools = client.tools_list();

    // Assert — the profile from postgres is reachable. This proves the
    // env-resolved password actually authenticated against the DB (a no-op
    // implementation that ignores profile_source would have an empty tools list).
    let tools_array = tools["result"]["tools"]
        .as_array()
        .expect("tools/list returns tools array");
    assert!(
        tools_array.iter().any(|t| t["name"].as_str() == Some("test")),
        "profile `test` from postgres should be visible when password is correctly resolved from env, got: {:?}",
        tools_array
    );
}

// ─── C-PS11: wrong password = startup failure ───

/// C-PS11: If `OSTIA_TEST_PG_PASSWORD` has the wrong value, the postgres
/// authentication fails and `ostia serve` exits non-zero.
#[tokio::test]
async fn postgres_provider_wrong_password_blocks_startup() {
    // Arrange
    mcp_common::assert_user_namespaces();
    assert_docker_available();
    let workspace = tempfile::tempdir().expect("create workspace");
    let ws_path = workspace.path().to_str().unwrap().to_string();

    let (_container, dsn) = start_seeded_postgres(&ws_path).await;

    let bootstrap = mcp_common::write_bootstrap_with_profile_source(&format!(
        r#"  provider: postgres
  dsn: "{dsn}"
  auth:
    type: static_secret
    password_env: OSTIA_TEST_PG_PASSWORD"#
    ));

    // Act — wrong password
    let outcome = mcp_common::spawn_or_capture_startup_failure(
        bootstrap.path(),
        &[("OSTIA_TEST_PG_PASSWORD", "definitely-wrong-password")],
    );

    // Assert
    match outcome {
        mcp_common::StdioStartupOutcome::ExitedEarly { status, stderr } => {
            assert!(
                !status.success(),
                "expected non-zero exit on auth failure, got: {:?}",
                status
            );
            let lower = stderr.to_lowercase();
            assert!(
                lower.contains("authentication")
                    || lower.contains("password")
                    || lower.contains("profile source"),
                "stderr should mention `authentication` / `password` / `profile source`, got: {:?}",
                stderr
            );
        }
        mcp_common::StdioStartupOutcome::Ready(_) => {
            panic!("expected ostia serve to exit on wrong postgres password");
        }
    }
}

// ─── C-PS12: connection refused = startup failure ───

/// C-PS12: When the postgres DSN points at a port nothing is listening on,
/// `ostia serve` exits non-zero with stderr mentioning the failure.
#[tokio::test]
async fn postgres_provider_connection_refused_blocks_startup() {
    // Arrange — pick a free port without binding it
    let port = free_port();
    let bootstrap = mcp_common::write_bootstrap_with_profile_source(&format!(
        r#"  provider: postgres
  dsn: "postgres://postgres@127.0.0.1:{port}/postgres"
  auth:
    type: static_secret
    password_env: OSTIA_TEST_PG_PASSWORD"#
    ));

    // Act
    let outcome = mcp_common::spawn_or_capture_startup_failure(
        bootstrap.path(),
        &[("OSTIA_TEST_PG_PASSWORD", "postgres")],
    );

    // Assert
    match outcome {
        mcp_common::StdioStartupOutcome::ExitedEarly { status, stderr } => {
            assert!(
                !status.success(),
                "expected non-zero exit on connection-refused, got: {:?}",
                status
            );
            let lower = stderr.to_lowercase();
            assert!(
                lower.contains("profile source")
                    || lower.contains("connection refused")
                    || lower.contains(&format!("{port}")),
                "stderr should mention `profile source` / `connection refused` / port, got: {:?}",
                stderr
            );
        }
        mcp_common::StdioStartupOutcome::Ready(_) => {
            panic!("expected ostia serve to exit when postgres is unreachable on port {port}");
        }
    }
}

// ─── C-PS15: postgres source reaches the real sandbox ───

/// C-PS15: A profile loaded from postgres reaches the real sandbox
/// enforcement layer. Verify by writing a file inside the workspace and
/// checking the file exists on the host afterward.
#[tokio::test]
async fn postgres_source_executes_command_through_real_sandbox() {
    // Arrange
    mcp_common::assert_user_namespaces();
    assert_docker_available();
    let workspace = tempfile::tempdir().expect("create workspace");
    let ws_path = workspace.path().to_str().unwrap().to_string();

    let (_container, dsn) = start_seeded_postgres(&ws_path).await;

    let bootstrap = mcp_common::write_bootstrap_with_profile_source(&format!(
        r#"  provider: postgres
  dsn: "{dsn}"
  auth:
    type: static_secret
    password_env: OSTIA_TEST_PG_PASSWORD"#
    ));

    let mut client = mcp_common::McpClient::spawn_with_args_and_env(
        bootstrap.path(),
        &[],
        &[("OSTIA_TEST_PG_PASSWORD", "postgres")],
    );
    client.handshake();

    // Act — write a file via the sandboxed profile
    let response = client.call_tool(
        "test",
        json!({
            "command": format!("echo pg-sandbox-proof > {}/marker.txt", ws_path)
        }),
    );

    // Assert — file is actually written to the host filesystem
    let result = &response["result"];
    let is_error = result["isError"].as_bool().unwrap_or(false);
    assert!(!is_error, "tools/call should not be isError, got: {:?}", result);

    let marker_path = workspace.path().join("marker.txt");
    let contents = std::fs::read_to_string(&marker_path)
        .expect("marker file should exist after postgres-sourced sandbox write");
    assert!(
        contents.contains("pg-sandbox-proof"),
        "sandbox-written file should contain `pg-sandbox-proof`, got: {:?}",
        contents
    );
}
