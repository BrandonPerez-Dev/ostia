//! Integration tests: binary-source lifecycle (Slice 3 of #11).
//! Covers C-BS10–C-BS15 from `spec/binary-source.md` — cold-cache blocking,
//! eager pull on Slice 2 diff, per-binary fail-open, missing-binary tool
//! call error, warm-cache no-refetch, sha256 mismatch.

mod mcp_common;

use serde_json::json;
use std::io::Write;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres as PostgresImage;

fn assert_docker_available() {
    let output = std::process::Command::new("docker").arg("info").output();
    let ok = matches!(output, Ok(o) if o.status.success());
    assert!(ok, "docker required for lifecycle postgres tests");
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

// ─── C-BS10 ───

/// C-BS10: A cold-cache tool call blocks until the binary is pulled, then
/// returns. The artificial download delay shows up as wall-clock latency.
#[test]
fn cold_cache_tool_call_blocks_until_pulled() {
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("workspace");
    let cache_dir = mcp_common::temp_binary_cache_dir();

    let bytes = mcp_common::make_fake_binary_bytes("cold-then-warm");
    let sha = mcp_common::sha256_hex(&bytes);
    let delay = Duration::from_millis(500);
    let port = mcp_common::start_delayed_binary_mock(bytes.clone(), delay);

    let config = format!(
        r#"binary_cache_dir: {cache_dir}

binaries:
  slowbin:
    sha256: "{sha}"
    format: binary
    source:
      provider: http
      url: "http://127.0.0.1:{port}/slowbin"

bundles:
  baseline:
    binaries: [sh, bash, slowbin]

profiles:
  test:
    bundles: [baseline]
    filesystem:
      workspace: {workspace}
"#,
        cache_dir = cache_dir.path().display(),
        workspace = workspace.path().display(),
    );
    let mut config_file = tempfile::NamedTempFile::new().expect("config temp");
    config_file.write_all(config.as_bytes()).expect("write config");

    let mut client = mcp_common::McpClient::spawn(config_file.path());
    client.handshake();

    // Act — first call blocks on download
    let started = Instant::now();
    let response = client.call_tool("test", json!({ "command": "slowbin" }));
    let elapsed = started.elapsed();

    // Assert — call returned successfully AND took at least the artificial delay
    let result = &response["result"];
    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("cold-then-warm"),
        "cold-cache call should return binary output, got: {:?}",
        text
    );
    assert!(
        elapsed >= delay,
        "cold-cache call should block at least {:?}, took {:?}",
        delay,
        elapsed
    );

    // Subsequent call: warm cache, should be much faster
    let started2 = Instant::now();
    let _ = client.call_tool("test", json!({ "command": "slowbin" }));
    let elapsed2 = started2.elapsed();
    assert!(
        elapsed2 < delay,
        "warm-cache call should be faster than cold ({:?} vs delay {:?})",
        elapsed2,
        delay
    );
}

// ─── C-BS11 ───

/// C-BS11: Eager pull on Slice 2 refresh — the binary is in the cache
/// BEFORE any tool call references it.
#[tokio::test]
async fn eager_pull_on_refresh_diff_stages_binary() {
    mcp_common::assert_user_namespaces();
    assert_docker_available();
    let workspace = tempfile::tempdir().expect("workspace");
    let ws_path = workspace.path().to_str().unwrap().to_string();
    let cache_dir = mcp_common::temp_binary_cache_dir();

    let bytes = mcp_common::make_fake_binary_bytes("eagerly-pulled");
    let sha = mcp_common::sha256_hex(&bytes);
    let (port, _counter) = mcp_common::start_counted_binary_mock(bytes.clone());

    let container = PostgresImage::default()
        .start()
        .await
        .expect("start postgres");
    let host = container.get_host().await.expect("host").to_string();
    let pg_port = container.get_host_port_ipv4(5432).await.expect("port");
    let live_dsn = format!("postgres://postgres:postgres@{host}:{pg_port}/postgres");
    let ostia_dsn = format!("postgres://postgres@{host}:{pg_port}/postgres");

    // Seed initial — only sh/bash bundle, no eager-bin yet
    {
        let (cli, h) = tokio_postgres::connect(&live_dsn, tokio_postgres::NoTls)
            .await
            .expect("connect");
        let task = tokio::spawn(async move {
            let _ = h.await;
        });
        cli.batch_execute(
            r#"
            CREATE TABLE bundles (name TEXT PRIMARY KEY, definition JSONB NOT NULL, updated_at TIMESTAMPTZ DEFAULT now());
            CREATE TABLE profiles (name TEXT PRIMARY KEY, definition JSONB NOT NULL, updated_at TIMESTAMPTZ DEFAULT now());
            CREATE TABLE binaries (name TEXT PRIMARY KEY, definition JSONB NOT NULL, updated_at TIMESTAMPTZ DEFAULT now());
            "#,
        )
        .await
        .expect("create schema");

        let baseline = json!({ "binaries": ["sh", "bash"] });
        cli.execute(
            "INSERT INTO bundles (name, definition) VALUES ($1, $2)",
            &[&"baseline", &baseline],
        )
        .await
        .expect("insert bundle");

        let profile_def = json!({
            "bundles": ["baseline"],
            "filesystem": { "workspace": ws_path }
        });
        cli.execute(
            "INSERT INTO profiles (name, definition) VALUES ($1, $2)",
            &[&"test", &profile_def],
        )
        .await
        .expect("insert profile");

        drop(cli);
        task.abort();
    }

    let config = format!(
        r#"binary_cache_dir: {cache_dir}

profile_source:
  provider: postgres
  dsn: "{ostia_dsn}"
  cache_ttl: 1s
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

    // Add a new binary to the registry + reference it in the bundle. Slice 2's
    // refresh tick should pick this up, compute the diff, and eager-pull.
    let (live, lh) = tokio_postgres::connect(&live_dsn, tokio_postgres::NoTls)
        .await
        .expect("live connect");
    let lt = tokio::spawn(async move {
        let _ = lh.await;
    });
    let bin_def = json!({
        "sha256": sha,
        "format": "binary",
        "source": { "provider": "http", "url": format!("http://127.0.0.1:{port}/eager") }
    });
    live.execute(
        "INSERT INTO binaries (name, definition) VALUES ($1, $2)",
        &[&"eager-bin", &bin_def],
    )
    .await
    .expect("insert binary");
    let updated_bundle = json!({ "binaries": ["sh", "bash", "eager-bin"] });
    live.execute(
        "UPDATE bundles SET definition = $1 WHERE name = $2",
        &[&updated_bundle, &"baseline"],
    )
    .await
    .expect("update bundle");
    drop(live);
    lt.abort();

    // Wait past TTL + a settle window for the eager pull to land on disk
    tokio::time::sleep(Duration::from_millis(1500)).await;

    // Trigger a refresh by calling tools/list (per the cache-on-call model).
    let _ = client.tools_list();

    // Give the eager pull a short window to download from the http mock
    tokio::time::sleep(Duration::from_millis(800)).await;

    // Assert — the cache file exists BEFORE any tool call references the new binary
    let cache_path = cache_dir.path().join(&sha).join("eager-bin");
    assert!(
        cache_path.exists(),
        "eager-bin should be in cache at {} after refresh + eager pull, before any tool call",
        cache_path.display()
    );

    drop(client);
    drop(container);
}

// ─── C-BS12 ───

/// C-BS12: One bad binary URL doesn't block other binaries' pulls. Loud
/// stderr warning. tools/call to the bad binary surfaces a clear error.
#[test]
fn single_bad_binary_does_not_block_others() {
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("workspace");
    let cache_dir = mcp_common::temp_binary_cache_dir();

    let good_bytes = mcp_common::make_fake_binary_bytes("good-binary-works");
    let good_sha = mcp_common::sha256_hex(&good_bytes);
    let (good_port, _good_counter) = mcp_common::start_counted_binary_mock(good_bytes.clone());

    let bad_port = mcp_common::start_failing_binary_mock(500);
    let bad_sha = "0".repeat(64); // claim some sha; mock returns 500 anyway

    let config = format!(
        r#"binary_cache_dir: {cache_dir}

binaries:
  good:
    sha256: "{good_sha}"
    format: binary
    source: {{ provider: http, url: "http://127.0.0.1:{good_port}/good" }}
  bad:
    sha256: "{bad_sha}"
    format: binary
    source: {{ provider: http, url: "http://127.0.0.1:{bad_port}/bad" }}

bundles:
  baseline:
    binaries: [sh, bash, good, bad]

profiles:
  test:
    bundles: [baseline]
    filesystem:
      workspace: {workspace}
"#,
        cache_dir = cache_dir.path().display(),
        workspace = workspace.path().display(),
    );
    let mut config_file = tempfile::NamedTempFile::new().expect("config temp");
    config_file.write_all(config.as_bytes()).expect("write config");

    let (mut client, stderr_buf) =
        mcp_common::spawn_with_stderr_capture(config_file.path(), &[], &[]);
    client.handshake();

    // Trigger pulls
    let _ = client.tools_list();
    std::thread::sleep(Duration::from_millis(800));

    // Act — good call should work; bad call should error cleanly
    let good_resp = client.call_tool("test", json!({ "command": "good" }));
    let good_text = mcp_common::get_content_text(&good_resp["result"]);
    assert!(
        good_text.contains("good-binary-works"),
        "good binary should run, got: {:?}",
        good_text
    );

    let bad_resp = client.call_tool("test", json!({ "command": "bad" }));
    let bad_result = &bad_resp["result"];
    let bad_is_error = bad_result["isError"].as_bool().unwrap_or(false);
    let bad_text = mcp_common::get_content_text(bad_result);
    assert!(
        bad_is_error,
        "bad binary call should be isError, got: {:?}",
        bad_result
    );
    assert!(
        bad_text.contains("bad"),
        "bad binary error should mention the name, got: {:?}",
        bad_text
    );

    // Assert — cache state
    let good_cache = cache_dir.path().join(&good_sha).join("good");
    let bad_cache = cache_dir.path().join(&bad_sha).join("bad");
    assert!(
        good_cache.exists(),
        "good cache file should exist at {}",
        good_cache.display()
    );
    assert!(
        !bad_cache.exists(),
        "bad cache file should NOT exist at {}",
        bad_cache.display()
    );

    // Assert — stderr warning
    let captured = stderr_buf.lock().unwrap().clone();
    let lower = captured.to_lowercase();
    assert!(
        lower.contains("binary source") || lower.contains("binary"),
        "stderr should mention `binary source` or `binary`, got: {:?}",
        captured
    );
    assert!(
        lower.contains("pull") || lower.contains("fetch") || lower.contains("failed"),
        "stderr should mention pull/fetch/failed for the bad binary, got: {:?}",
        captured
    );
}

// ─── C-BS13 ───

/// C-BS13: tools/call to a binary that couldn't be fetched returns a clean
/// error — no panic, no hang.
#[test]
fn tool_call_to_uncached_binary_returns_clear_error() {
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("workspace");
    let cache_dir = mcp_common::temp_binary_cache_dir();

    let port = free_port();
    let sha = "f".repeat(64);

    let config = format!(
        r#"binary_cache_dir: {cache_dir}

binaries:
  unreachable:
    sha256: "{sha}"
    format: binary
    source: {{ provider: http, url: "http://127.0.0.1:{port}/unreachable" }}

bundles:
  baseline:
    binaries: [sh, bash, unreachable]

profiles:
  test:
    bundles: [baseline]
    filesystem:
      workspace: {workspace}
"#,
        cache_dir = cache_dir.path().display(),
        workspace = workspace.path().display(),
    );
    let mut config_file = tempfile::NamedTempFile::new().expect("config temp");
    config_file.write_all(config.as_bytes()).expect("write config");

    let mut client = mcp_common::McpClient::spawn(config_file.path());
    client.handshake();
    let _ = client.tools_list();
    std::thread::sleep(Duration::from_millis(300));

    // Act — call should not hang
    let response = client.call_tool("test", json!({ "command": "unreachable" }));
    let result = &response["result"];

    // Assert
    let is_error = result["isError"].as_bool().unwrap_or(false);
    assert!(is_error, "call to unreachable binary should error, got: {:?}", result);
    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("unreachable"),
        "error should name the binary, got: {:?}",
        text
    );
}

// ─── C-BS14 ───

/// C-BS14: After warm cache, repeated tools/call invocations do NOT refetch
/// the binary from the source. Counted mock asserts zero additional GETs.
#[test]
fn warm_cache_tools_call_does_not_refetch() {
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("workspace");
    let cache_dir = mcp_common::temp_binary_cache_dir();

    let bytes = mcp_common::make_fake_binary_bytes("warm");
    let sha = mcp_common::sha256_hex(&bytes);
    let (port, counter) = mcp_common::start_counted_binary_mock(bytes.clone());

    let config = format!(
        r#"binary_cache_dir: {cache_dir}

binaries:
  warmbin:
    sha256: "{sha}"
    format: binary
    source: {{ provider: http, url: "http://127.0.0.1:{port}/warm" }}

bundles:
  baseline:
    binaries: [sh, bash, warmbin]

profiles:
  test:
    bundles: [baseline]
    filesystem:
      workspace: {workspace}
"#,
        cache_dir = cache_dir.path().display(),
        workspace = workspace.path().display(),
    );
    let mut config_file = tempfile::NamedTempFile::new().expect("config temp");
    config_file.write_all(config.as_bytes()).expect("write config");

    let mut client = mcp_common::McpClient::spawn(config_file.path());
    client.handshake();

    // First call primes the cache
    let _ = client.call_tool("test", json!({ "command": "warmbin" }));
    let pulls_after_first = counter.load(Ordering::SeqCst);
    assert!(
        pulls_after_first >= 1,
        "first call should pull at least once, got {}",
        pulls_after_first
    );

    // Five more calls — must not re-pull
    for _ in 0..5 {
        let _ = client.call_tool("test", json!({ "command": "warmbin" }));
    }
    let pulls_after_warm = counter.load(Ordering::SeqCst);
    assert_eq!(
        pulls_after_warm, pulls_after_first,
        "warm-cache calls should not refetch, got {} additional GETs",
        pulls_after_warm - pulls_after_first
    );
}

// ─── C-BS15 ───

/// C-BS15: sha256 mismatch on download is treated as a fetch failure.
/// Cache stays empty; tools/call surfaces a clear error.
#[test]
fn sha256_mismatch_treated_as_fetch_failure() {
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("workspace");
    let cache_dir = mcp_common::temp_binary_cache_dir();

    let actual_bytes = mcp_common::make_fake_binary_bytes("actually-served");
    let (port, _) = mcp_common::start_counted_binary_mock(actual_bytes.clone());

    // Claim a different sha than the actual bytes
    let claimed_sha = "0".repeat(64);
    let actual_sha = mcp_common::sha256_hex(&actual_bytes);
    assert_ne!(claimed_sha, actual_sha);

    let config = format!(
        r#"binary_cache_dir: {cache_dir}

binaries:
  badsha:
    sha256: "{claimed_sha}"
    format: binary
    source: {{ provider: http, url: "http://127.0.0.1:{port}/badsha" }}

bundles:
  baseline:
    binaries: [sh, bash, badsha]

profiles:
  test:
    bundles: [baseline]
    filesystem:
      workspace: {workspace}
"#,
        cache_dir = cache_dir.path().display(),
        workspace = workspace.path().display(),
    );
    let mut config_file = tempfile::NamedTempFile::new().expect("config temp");
    config_file.write_all(config.as_bytes()).expect("write config");

    let (mut client, stderr_buf) =
        mcp_common::spawn_with_stderr_capture(config_file.path(), &[], &[]);
    client.handshake();
    let _ = client.tools_list();
    std::thread::sleep(Duration::from_millis(800));

    let response = client.call_tool("test", json!({ "command": "badsha" }));
    let result = &response["result"];
    let is_error = result["isError"].as_bool().unwrap_or(false);
    assert!(
        is_error,
        "sha mismatch should produce isError on tools/call, got: {:?}",
        result
    );

    let cache_path = cache_dir.path().join(&claimed_sha).join("badsha");
    assert!(
        !cache_path.exists(),
        "cache file should NOT exist when sha mismatched, got: {}",
        cache_path.display()
    );

    let captured = stderr_buf.lock().unwrap().clone();
    let lower = captured.to_lowercase();
    assert!(
        lower.contains("badsha"),
        "stderr should mention the binary name, got: {:?}",
        captured
    );
    assert!(
        lower.contains("sha256") || lower.contains("integrity") || lower.contains("mismatch"),
        "stderr should mention sha256/integrity/mismatch, got: {:?}",
        captured
    );
}
