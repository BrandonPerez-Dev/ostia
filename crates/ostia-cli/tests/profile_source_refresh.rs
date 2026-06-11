//! Integration tests: profile-source live resolution + TTL cache (Slice 2 of #11).
//!
//! Covers contracts C-PS16, C-PS17, C-PS18, C-PS19, C-PS21 from
//! `spec/profile-source.md`. Slice 2 is the live-data slice — bundles + profiles
//! are cached in-process for `cache_ttl` (default 30s) and re-fetched after
//! expiry. Refresh failures after a successful initial load are fail-open
//! (last-good config keeps serving + loud stderr warning).
//!
//! Tests use SHORT TTLs (1-2s) to keep wall clock acceptable. Postgres tests
//! use `#[tokio::test]` so the runtime stays alive across testcontainers'
//! async drop (same pattern as Slice 1's postgres tests).

mod mcp_common;

use serde_json::json;
use std::sync::atomic::Ordering;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres as PostgresImage;

// ─── Test infrastructure shared with Slice 1 ───

fn assert_docker_available() {
    let output = std::process::Command::new("docker").arg("info").output();
    let ok = matches!(output, Ok(o) if o.status.success());
    assert!(
        ok,
        "docker is required for profile_source_refresh postgres tests but is not available. \
         install docker and ensure the current user can run `docker info`."
    );
}

/// Minimal YAML profile config the mock servers return. Defines one bundle
/// `baseline` with sh/bash/echo/cat/ls and one profile with the given name.
fn yaml_with_profile(workspace: &str, profile_name: &str) -> String {
    format!(
        r#"bundles:
  baseline:
    binaries: [sh, bash, echo, cat, ls]

profiles:
  {profile_name}:
    bundles: [baseline]
    filesystem:
      workspace: {workspace}
"#
    )
}

/// Apply Slice 1 schema and insert a single bundle + single profile. Used by
/// C-PS18 and C-PS19 to set up the initial postgres state.
async fn seed_database_initial(dsn: &str, workspace: &str, profile_name: &str) {
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
    let profile = json!({
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
            &[&profile_name, &profile],
        )
        .await
        .expect("insert initial profile");

    drop(client);
    connection_handle.abort();
}

/// Open a separate tokio-postgres client for the test to mutate the DB live
/// (INSERT / DELETE) while ostia is running.
async fn live_client(dsn: &str) -> (tokio_postgres::Client, tokio::task::JoinHandle<()>) {
    let (client, connection) = tokio_postgres::connect(dsn, tokio_postgres::NoTls)
        .await
        .expect("open live test postgres client");
    let handle = tokio::spawn(async move {
        let _ = connection.await;
    });
    (client, handle)
}

async fn start_seeded_postgres(
    workspace: &str,
    profile_name: &str,
) -> (ContainerAsync<PostgresImage>, String) {
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

    let full_dsn = format!("postgres://postgres:postgres@{host}:{port}/postgres");
    seed_database_initial(&full_dsn, workspace, profile_name).await;

    let dsn_without_password = format!("postgres://postgres@{host}:{port}/postgres");
    (container, dsn_without_password)
}

fn list_profile_names(tools_response: &serde_json::Value) -> Vec<String> {
    tools_response["result"]["tools"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t["name"].as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

// ─── C-PS16: cache hit within TTL prevents refetch ───

/// C-PS16: With cache_ttl 10s, two tools/list calls in quick succession
/// should result in exactly one source GET (the initial load).
#[test]
fn http_source_within_ttl_does_not_refetch() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let yaml = yaml_with_profile(workspace.path().to_str().unwrap(), "test");

    let (port, counter) =
        mcp_common::start_counted_mock_server(yaml, "application/yaml");

    let bootstrap = mcp_common::write_bootstrap_with_profile_source(&format!(
        r#"  provider: http
  url: "http://127.0.0.1:{port}/profiles"
  cache_ttl: 10s
  auth:
    type: none"#
    ));

    let mut client = mcp_common::McpClient::spawn(bootstrap.path());
    client.handshake();

    // Act — two tools/list calls in immediate succession
    let _first = client.tools_list();
    let _second = client.tools_list();

    // Brief settle window so any in-flight refresh can land before we read.
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Assert
    let total_gets = counter.load(Ordering::SeqCst);
    assert_eq!(
        total_gets, 1,
        "mock server should have received exactly 1 GET after handshake + 2 quick tools/list within TTL, got {total_gets}"
    );
}

// ─── C-PS17: cache expires after TTL ───

/// C-PS17: With cache_ttl 1s, modifying the source body and waiting > TTL
/// causes the next tools/list to see the new data.
#[test]
fn http_source_after_ttl_refetches_and_sees_new_data() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let ws = workspace.path().to_str().unwrap().to_string();
    let before_yaml = yaml_with_profile(&ws, "before");

    let (port, state) =
        mcp_common::start_stateful_mock_server(before_yaml, "application/yaml");

    let bootstrap = mcp_common::write_bootstrap_with_profile_source(&format!(
        r#"  provider: http
  url: "http://127.0.0.1:{port}/profiles"
  cache_ttl: 1s
  auth:
    type: none"#
    ));

    let mut client = mcp_common::McpClient::spawn(bootstrap.path());
    client.handshake();

    let first = client.tools_list();
    let first_names = list_profile_names(&first);
    assert!(
        first_names.iter().any(|n| n == "before"),
        "first tools/list should contain `before`, got {:?}",
        first_names
    );
    assert!(
        !first_names.iter().any(|n| n == "after"),
        "first tools/list should NOT contain `after` yet, got {:?}",
        first_names
    );

    // Act — swap the mock's body, wait past TTL, ask again
    state.set(
        yaml_with_profile(&ws, "after"),
        200,
        "application/yaml",
    );
    std::thread::sleep(std::time::Duration::from_millis(1200));

    let second = client.tools_list();
    let second_names = list_profile_names(&second);

    // Assert
    assert!(
        second_names.iter().any(|n| n == "after"),
        "after TTL expiry, second tools/list should contain `after`, got {:?}",
        second_names
    );
    assert!(
        !second_names.iter().any(|n| n == "before"),
        "after TTL expiry, second tools/list should NOT contain stale `before`, got {:?}",
        second_names
    );
}

// ─── C-PS18: live profile addition (postgres) ───

/// C-PS18: Insert a new profile row into the live source while ostia is
/// running. After TTL expires, the new profile appears in tools/list.
#[tokio::test]
async fn postgres_source_picks_up_new_profile_after_ttl() {
    // Arrange
    mcp_common::assert_user_namespaces();
    assert_docker_available();
    let workspace = tempfile::tempdir().expect("create workspace");
    let ws = workspace.path().to_str().unwrap().to_string();

    let (_container, dsn) = start_seeded_postgres(&ws, "before").await;
    // The dsn returned has no password; build the password-bearing dsn for our
    // live test client.
    let live_dsn = dsn.replace("postgres@", "postgres:postgres@");

    let bootstrap = mcp_common::write_bootstrap_with_profile_source(&format!(
        r#"  provider: postgres
  dsn: "{dsn}"
  cache_ttl: 1s
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

    let first = client.tools_list();
    let first_names = list_profile_names(&first);
    assert!(
        first_names.iter().any(|n| n == "before"),
        "first tools/list should contain seeded `before` profile, got {:?}",
        first_names
    );
    assert!(
        !first_names.iter().any(|n| n == "after"),
        "first tools/list should NOT contain `after` yet, got {:?}",
        first_names
    );

    // Act — independent client inserts a new profile, then wait past TTL
    let (live, live_handle) = live_client(&live_dsn).await;
    let new_profile_def = json!({
        "bundles": ["baseline"],
        "filesystem": { "workspace": ws }
    });
    live.execute(
        "INSERT INTO profiles (name, definition) VALUES ($1, $2)",
        &[&"after", &new_profile_def],
    )
    .await
    .expect("insert live `after` profile");
    drop(live);
    live_handle.abort();

    std::thread::sleep(std::time::Duration::from_millis(1200));

    let second = client.tools_list();
    let second_names = list_profile_names(&second);

    // Assert
    assert!(
        second_names.iter().any(|n| n == "after"),
        "after live INSERT + TTL expiry, tools/list should include `after`, got {:?}",
        second_names
    );
    assert!(
        second_names.iter().any(|n| n == "before"),
        "tools/list should still include `before` (we only added a profile, didn't remove one), got {:?}",
        second_names
    );
}

// ─── C-PS19: live profile removal (postgres) ───

/// C-PS19: DELETE a profile row from the live source while ostia is running.
/// After TTL expires, the removed profile is no longer in tools/list.
#[tokio::test]
async fn postgres_source_picks_up_deleted_profile_after_ttl() {
    // Arrange — seed with `alpha`, then add `beta` so we start with two
    mcp_common::assert_user_namespaces();
    assert_docker_available();
    let workspace = tempfile::tempdir().expect("create workspace");
    let ws = workspace.path().to_str().unwrap().to_string();

    let (_container, dsn) = start_seeded_postgres(&ws, "alpha").await;
    let live_dsn = dsn.replace("postgres@", "postgres:postgres@");

    let (live_setup, setup_handle) = live_client(&live_dsn).await;
    let beta_def = json!({
        "bundles": ["baseline"],
        "filesystem": { "workspace": ws }
    });
    live_setup
        .execute(
            "INSERT INTO profiles (name, definition) VALUES ($1, $2)",
            &[&"beta", &beta_def],
        )
        .await
        .expect("insert beta profile");
    drop(live_setup);
    setup_handle.abort();

    let bootstrap = mcp_common::write_bootstrap_with_profile_source(&format!(
        r#"  provider: postgres
  dsn: "{dsn}"
  cache_ttl: 1s
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

    let first = client.tools_list();
    let first_names = list_profile_names(&first);
    assert!(
        first_names.iter().any(|n| n == "alpha")
            && first_names.iter().any(|n| n == "beta"),
        "initial tools/list should contain both alpha and beta, got {:?}",
        first_names
    );

    // Act — delete beta via a separate client, wait past TTL
    let (live_del, del_handle) = live_client(&live_dsn).await;
    live_del
        .execute("DELETE FROM profiles WHERE name = $1", &[&"beta"])
        .await
        .expect("delete beta profile");
    drop(live_del);
    del_handle.abort();

    std::thread::sleep(std::time::Duration::from_millis(1200));

    let second = client.tools_list();
    let second_names = list_profile_names(&second);

    // Assert
    assert!(
        second_names.iter().any(|n| n == "alpha"),
        "tools/list should still contain `alpha`, got {:?}",
        second_names
    );
    assert!(
        !second_names.iter().any(|n| n == "beta"),
        "tools/list should NOT contain `beta` after deletion + TTL expiry, got {:?}",
        second_names
    );
}

// ─── C-PS21: refresh failure is fail-open with loud warning ───

/// C-PS21: When the source goes down AFTER a successful initial load,
/// ostia must NOT shut down. It keeps serving the last-good config and
/// logs a warning to stderr containing `profile source` + `refresh`.
#[test]
fn http_source_refresh_failure_is_fail_open() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let ws = workspace.path().to_str().unwrap().to_string();
    let yaml = yaml_with_profile(&ws, "loaded");

    let (port, state) = mcp_common::start_stateful_mock_server(yaml, "application/yaml");

    let bootstrap = mcp_common::write_bootstrap_with_profile_source(&format!(
        r#"  provider: http
  url: "http://127.0.0.1:{port}/profiles"
  cache_ttl: 1s
  auth:
    type: none"#
    ));

    let (mut client, stderr_buf) =
        mcp_common::spawn_with_stderr_capture(bootstrap.path(), &[], &[]);
    client.handshake();

    let first = client.tools_list();
    let first_names = list_profile_names(&first);
    assert!(
        first_names.iter().any(|n| n == "loaded"),
        "initial tools/list should contain `loaded`, got {:?}",
        first_names
    );

    // Act — flip mock to 500, wait past TTL, ask again
    state.set("internal server error".to_string(), 500, "text/plain");
    std::thread::sleep(std::time::Duration::from_millis(1200));

    let second = client.tools_list();
    let second_names = list_profile_names(&second);

    // Brief window so the warning has time to flush to stderr
    std::thread::sleep(std::time::Duration::from_millis(200));

    // Assert — server is still up serving last-good config
    assert!(
        second_names.iter().any(|n| n == "loaded"),
        "after refresh failure, tools/list should STILL contain last-good `loaded`, got {:?}",
        second_names
    );

    // Assert — stderr warning was emitted
    let captured = stderr_buf.lock().unwrap().clone();
    let lower = captured.to_lowercase();
    assert!(
        lower.contains("profile source"),
        "stderr should mention `profile source` to identify the failure source, got: {:?}",
        captured
    );
    assert!(
        lower.contains("refresh"),
        "stderr should mention `refresh` to distinguish from initial-load failure, got: {:?}",
        captured
    );
}
