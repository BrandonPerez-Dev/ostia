//! Integration tests: binary-source HTTP provider (Slice 3 of #11).
//! Covers C-BS6 (single binary) and C-BS7 (tar.gz tarball) from
//! `spec/binary-source.md`.

mod mcp_common;

use serde_json::json;
use std::io::Write;

// ─── C-BS6 ───

/// C-BS6: HTTP source serving a single binary downloads, sha-verifies, and
/// bind-mounts into the sandbox.
#[test]
fn http_source_single_binary_lands_in_sandbox() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let cache_dir = mcp_common::temp_binary_cache_dir();
    let bytes = mcp_common::make_fake_binary_bytes("via-http");
    let sha = mcp_common::sha256_hex(&bytes);

    let (port, counter) = mcp_common::start_counted_binary_mock(bytes.clone());

    let config = format!(
        r#"binary_cache_dir: {cache_dir}

binaries:
  gh:
    sha256: "{sha}"
    format: binary
    source:
      provider: http
      url: "http://127.0.0.1:{port}/gh"

bundles:
  baseline:
    binaries: [sh, bash, gh]

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

    // Act
    let response = client.call_tool("test", json!({ "command": "gh" }));

    // Assert — output matches the fake binary
    let result = &response["result"];
    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("via-http"),
        "tools/call should run the http-sourced binary, got: {:?}",
        text
    );

    // Assert — cache file lands at the content-addressed path
    let cache_path = cache_dir.path().join(&sha).join("gh");
    assert!(
        cache_path.exists(),
        "cache file should land at {} after http pull",
        cache_path.display()
    );

    // Assert — the mock received at least one GET
    use std::sync::atomic::Ordering;
    assert!(
        counter.load(Ordering::SeqCst) >= 1,
        "mock should have received at least 1 GET"
    );
}

// ─── C-BS7 ───

/// C-BS7: HTTP source serving a .tar.gz extracts the entry binary and
/// bind-mounts it into the sandbox.
#[test]
fn http_source_tarball_extracts_entry_binary() {
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let cache_dir = mcp_common::temp_binary_cache_dir();

    let entry_bytes = mcp_common::make_fake_binary_bytes("via-tarball");
    let tarball = mcp_common::make_fake_tarball("jq", &entry_bytes);
    let tarball_sha = mcp_common::sha256_hex(&tarball);

    let (port, _counter) = mcp_common::start_counted_binary_mock(tarball.clone());

    let config = format!(
        r#"binary_cache_dir: {cache_dir}

binaries:
  jq:
    sha256: "{tarball_sha}"
    format: tar.gz
    entry: bin/jq
    source:
      provider: http
      url: "http://127.0.0.1:{port}/jq.tar.gz"

bundles:
  baseline:
    binaries: [sh, bash, jq]

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

    // Act
    let response = client.call_tool("test", json!({ "command": "jq" }));

    // Assert
    let result = &response["result"];
    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("via-tarball"),
        "tarball entry should run, got: {:?}",
        text
    );

    let entry_path = cache_dir.path().join(&tarball_sha).join("bin").join("jq");
    assert!(
        entry_path.exists(),
        "tarball entry should be extracted to {}",
        entry_path.display()
    );
}
