//! Integration tests: binary-source registry shape + bundle resolution
//! (Slice 3 of #11). Covers C-BS1, C-BS2, C-BS3, C-BS4, C-BS5, C-BS16 from
//! `spec/binary-source.md`.
//!
//! These tests focus on the static parts of Slice 3: top-level `binaries:`
//! registry, inline-ref override, host-PATH fallback, multi-version, conflict
//! detection, and backwards compat with Slice 2 configs.

mod mcp_common;

use serde_json::json;
use std::io::Write;

// ─── C-BS1 ───

/// C-BS1: Top-level `binaries:` registry parses and a bundle resolves a
/// registered binary. The cache file lands at <binary_cache_dir>/<sha>/<name>
/// after the tool call uses it.
#[test]
fn binary_registry_parses_and_resolves_registered_name() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let cache_dir = mcp_common::temp_binary_cache_dir();
    let bytes = mcp_common::make_fake_binary_bytes("gh-from-registry");
    let sha = mcp_common::sha256_hex(&bytes);

    // Write the fixture binary to a file that the file-provider source will read
    let fixture_path = workspace.path().join("gh-fixture");
    std::fs::write(&fixture_path, &bytes).expect("write fixture");

    // Bootstrap YAML — inline source (no profile_source block), with a
    // top-level binaries: registry and a bundle that references `gh` by name.
    let config = format!(
        r#"binary_cache_dir: {}

binaries:
  gh:
    sha256: "{sha}"
    format: binary
    source:
      provider: file
      path: {}

bundles:
  baseline:
    binaries: [sh, bash, gh]

profiles:
  test:
    bundles: [baseline]
    filesystem:
      workspace: {}
"#,
        cache_dir.path().display(),
        fixture_path.display(),
        workspace.path().display()
    );
    let mut config_file = tempfile::NamedTempFile::new().expect("config temp");
    config_file.write_all(config.as_bytes()).expect("write config");

    let mut client = mcp_common::McpClient::spawn(config_file.path());
    client.handshake();

    // Act
    let response = client.call_tool("test", json!({ "command": "gh" }));

    // Assert — command output matches the fixture's echo
    let result = &response["result"];
    let is_error = result["isError"].as_bool().unwrap_or(false);
    assert!(!is_error, "tools/call should not be isError, got: {:?}", result);
    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("gh-from-registry"),
        "registry-resolved gh should run the registered fixture, got: {:?}",
        text
    );

    // Assert — cache file exists at the content-addressed path
    let cache_path = cache_dir.path().join(&sha).join("gh");
    assert!(
        cache_path.exists(),
        "cache file should exist at {} after tool call",
        cache_path.display()
    );
}

// ─── C-BS2 ───

/// C-BS2: A bundle inline ref takes precedence over the registry entry of
/// the same name. Two different shas in play; the inline one is what runs.
#[test]
fn inline_ref_overrides_registry_for_same_name() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let cache_dir = mcp_common::temp_binary_cache_dir();

    let registry_bytes = mcp_common::make_fake_binary_bytes("registry-version");
    let registry_sha = mcp_common::sha256_hex(&registry_bytes);
    let registry_path = workspace.path().join("gh-registry");
    std::fs::write(&registry_path, &registry_bytes).expect("write registry fixture");

    let inline_bytes = mcp_common::make_fake_binary_bytes("inline-version");
    let inline_sha = mcp_common::sha256_hex(&inline_bytes);
    let inline_path = workspace.path().join("gh-inline");
    std::fs::write(&inline_path, &inline_bytes).expect("write inline fixture");

    assert_ne!(registry_sha, inline_sha, "test setup: fixtures must differ");

    let config = format!(
        r#"binary_cache_dir: {}

binaries:
  gh:
    sha256: "{registry_sha}"
    format: binary
    source:
      provider: file
      path: {}

bundles:
  baseline:
    binaries:
      - sh
      - bash
      - name: gh
        sha256: "{inline_sha}"
        format: binary
        source:
          provider: file
          path: {}

profiles:
  test:
    bundles: [baseline]
    filesystem:
      workspace: {}
"#,
        cache_dir.path().display(),
        registry_path.display(),
        inline_path.display(),
        workspace.path().display()
    );
    let mut config_file = tempfile::NamedTempFile::new().expect("config temp");
    config_file.write_all(config.as_bytes()).expect("write config");

    let mut client = mcp_common::McpClient::spawn(config_file.path());
    client.handshake();

    // Act
    let response = client.call_tool("test", json!({ "command": "gh" }));

    // Assert — inline version wins
    let result = &response["result"];
    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("inline-version"),
        "inline ref should override the registry entry, got: {:?}",
        text
    );
    assert!(
        !text.contains("registry-version"),
        "registry version should NOT have run, got: {:?}",
        text
    );

    let inline_cache = cache_dir.path().join(&inline_sha).join("gh");
    assert!(
        inline_cache.exists(),
        "inline-sha cache file should exist at {}",
        inline_cache.display()
    );
}

// ─── C-BS3 ───

/// C-BS3: A plain-name binary not in the registry falls back to host PATH
/// (preserves today's behavior for built-ins like `echo`).
#[test]
fn plain_name_without_registry_falls_back_to_path() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let cache_dir = mcp_common::temp_binary_cache_dir();
    let gh_bytes = mcp_common::make_fake_binary_bytes("gh-via-registry");
    let gh_sha = mcp_common::sha256_hex(&gh_bytes);
    let gh_path = workspace.path().join("gh-fixture");
    std::fs::write(&gh_path, &gh_bytes).expect("write gh fixture");

    let config = format!(
        r#"binary_cache_dir: {}

binaries:
  gh:
    sha256: "{gh_sha}"
    format: binary
    source:
      provider: file
      path: {}

bundles:
  baseline:
    binaries: [sh, bash, echo, gh]

profiles:
  test:
    bundles: [baseline]
    filesystem:
      workspace: {}
"#,
        cache_dir.path().display(),
        gh_path.display(),
        workspace.path().display()
    );
    let mut config_file = tempfile::NamedTempFile::new().expect("config temp");
    config_file.write_all(config.as_bytes()).expect("write config");

    let mut client = mcp_common::McpClient::spawn(config_file.path());
    client.handshake();

    // Act — echo is NOT in the registry; must resolve via host PATH
    let response = client.call_tool("test", json!({ "command": "echo path-fallback-works" }));

    // Assert
    let result = &response["result"];
    let is_error = result["isError"].as_bool().unwrap_or(false);
    assert!(!is_error, "tools/call should not be isError, got: {:?}", result);
    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("path-fallback-works"),
        "plain-name fallback to host PATH should run echo, got: {:?}",
        text
    );
}

// ─── C-BS4 ───

/// C-BS4: Cross-profile multi-version. Two profiles each declare their own
/// inline `gh` with different shas. Both work, both end up in cache.
#[test]
fn cross_profile_multi_version_both_work() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let cache_dir = mcp_common::temp_binary_cache_dir();

    let v1_bytes = mcp_common::make_fake_binary_bytes("version-1");
    let v1_sha = mcp_common::sha256_hex(&v1_bytes);
    let v1_path = workspace.path().join("gh-v1");
    std::fs::write(&v1_path, &v1_bytes).expect("write v1");

    let v2_bytes = mcp_common::make_fake_binary_bytes("version-2");
    let v2_sha = mcp_common::sha256_hex(&v2_bytes);
    let v2_path = workspace.path().join("gh-v2");
    std::fs::write(&v2_path, &v2_bytes).expect("write v2");

    let config = format!(
        r#"binary_cache_dir: {cache_dir}

bundles:
  alpha-tools:
    binaries:
      - sh
      - bash
      - name: gh
        sha256: "{v1_sha}"
        format: binary
        source: {{ provider: file, path: {v1_path} }}
  beta-tools:
    binaries:
      - sh
      - bash
      - name: gh
        sha256: "{v2_sha}"
        format: binary
        source: {{ provider: file, path: {v2_path} }}

profiles:
  alpha:
    bundles: [alpha-tools]
    filesystem:
      workspace: {workspace}
  beta:
    bundles: [beta-tools]
    filesystem:
      workspace: {workspace}
"#,
        cache_dir = cache_dir.path().display(),
        v1_path = v1_path.display(),
        v2_path = v2_path.display(),
        workspace = workspace.path().display(),
    );
    let mut config_file = tempfile::NamedTempFile::new().expect("config temp");
    config_file.write_all(config.as_bytes()).expect("write config");

    let mut client = mcp_common::McpClient::spawn(config_file.path());
    client.handshake();

    // Act
    let alpha_resp = client.call_tool("alpha", json!({ "command": "gh" }));
    let beta_resp = client.call_tool("beta", json!({ "command": "gh" }));

    // Assert
    let alpha_text = mcp_common::get_content_text(&alpha_resp["result"]);
    assert!(
        alpha_text.contains("version-1"),
        "alpha should run v1, got: {:?}",
        alpha_text
    );
    let beta_text = mcp_common::get_content_text(&beta_resp["result"]);
    assert!(
        beta_text.contains("version-2"),
        "beta should run v2, got: {:?}",
        beta_text
    );

    assert!(
        cache_dir.path().join(&v1_sha).join("gh").exists(),
        "v1 cache entry should exist"
    );
    assert!(
        cache_dir.path().join(&v2_sha).join("gh").exists(),
        "v2 cache entry should exist"
    );
}

// ─── C-BS5 ───

/// C-BS5: Within-profile conflict — two bundles in the same profile declare
/// `gh` with different shas. Startup is a clean error.
#[test]
fn within_profile_conflicting_shas_blocks_startup() {
    let workspace = tempfile::tempdir().expect("create workspace");
    let cache_dir = mcp_common::temp_binary_cache_dir();

    let a_bytes = mcp_common::make_fake_binary_bytes("a");
    let a_sha = mcp_common::sha256_hex(&a_bytes);
    let b_bytes = mcp_common::make_fake_binary_bytes("b");
    let b_sha = mcp_common::sha256_hex(&b_bytes);
    let a_path = workspace.path().join("gh-a");
    std::fs::write(&a_path, &a_bytes).expect("write a");
    let b_path = workspace.path().join("gh-b");
    std::fs::write(&b_path, &b_bytes).expect("write b");

    let config = format!(
        r#"binary_cache_dir: {cache_dir}

bundles:
  bundle-a:
    binaries:
      - sh
      - name: gh
        sha256: "{a_sha}"
        format: binary
        source: {{ provider: file, path: {a_path} }}
  bundle-b:
    binaries:
      - sh
      - name: gh
        sha256: "{b_sha}"
        format: binary
        source: {{ provider: file, path: {b_path} }}

profiles:
  test:
    bundles: [bundle-a, bundle-b]
    filesystem:
      workspace: {workspace}
"#,
        cache_dir = cache_dir.path().display(),
        a_path = a_path.display(),
        b_path = b_path.display(),
        workspace = workspace.path().display(),
    );
    let mut config_file = tempfile::NamedTempFile::new().expect("config temp");
    config_file.write_all(config.as_bytes()).expect("write config");

    // Act
    let outcome = mcp_common::spawn_or_capture_startup_failure(config_file.path(), &[]);

    // Assert
    match outcome {
        mcp_common::StdioStartupOutcome::ExitedEarly { status, stderr } => {
            assert!(
                !status.success(),
                "expected non-zero exit on within-profile conflict, got: {:?}",
                status
            );
            let lower = stderr.to_lowercase();
            assert!(
                lower.contains("gh"),
                "stderr should mention the conflicting binary name `gh`, got: {:?}",
                stderr
            );
            assert!(
                lower.contains("conflict") || lower.contains("inconsistent") || lower.contains("mismatch"),
                "stderr should mention conflict/inconsistent/mismatch, got: {:?}",
                stderr
            );
        }
        mcp_common::StdioStartupOutcome::Ready(_) => {
            panic!("expected startup to fail on within-profile sha conflict");
        }
    }
}

// ─── C-BS16 ───

/// C-BS16: Backwards compat — a Slice 2 config (no `binaries:` registry,
/// no `binary_cache_dir:`) still works. All binaries resolve via host PATH.
#[test]
fn slice_2_config_without_binaries_registry_still_works() {
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_mcp_config(workspace.path().to_str().unwrap(), &[]);

    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    let response = client.call_tool("test", json!({ "command": "echo backwards-compat-ok" }));

    let result = &response["result"];
    let is_error = result["isError"].as_bool().unwrap_or(false);
    assert!(!is_error, "tools/call should not be isError, got: {:?}", result);
    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("backwards-compat-ok"),
        "Slice 2 config should keep working unchanged, got: {:?}",
        text
    );
}
