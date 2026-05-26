//! Integration tests: profile-source `file` provider (Slice 1 of #11).
//!
//! Covers contracts C-PS1, C-PS2, C-PS3, C-PS13 from `spec/profile-source.md`.
//! The `file` provider is the implicit default when no `profile_source:` block
//! is present in the bootstrap `--config`, and is also available as an explicit
//! provider that loads from a *different* YAML file than the bootstrap.

mod mcp_common;

use serde_json::json;

// ─── C-PS1: legacy YAML still works (implicit default = file provider) ───

/// C-PS1: A bootstrap config without a `profile_source:` block must parse and
/// behave exactly as it did before Slice 1 — bundles + profiles inline, file
/// provider implicit.
///
/// NOTE: This test is GREEN today and is expected to stay green. It is the
/// backwards-compat enforcement contract. If this test ever fails, V0a has
/// regressed the implicit file-provider default.
#[test]
fn legacy_yaml_config_no_profile_source_block_runs_unchanged() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_mcp_config(workspace.path().to_str().unwrap(), &[]);

    let mut client = mcp_common::McpClient::spawn(config.path());

    // Act — full handshake + tools/list + tools/call
    let init = client.handshake();
    let tools = client.tools_list();
    let response = client.call_tool("test", json!({ "command": "echo hello" }));

    // Assert — handshake succeeds, the inline `test` profile is visible, command runs
    assert!(
        init["result"]["serverInfo"]["name"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .contains("ostia"),
        "initialize should return ostia serverInfo, got: {:?}",
        init
    );

    let tools_array = tools["result"]["tools"]
        .as_array()
        .expect("tools/list returns tools array");
    assert!(
        tools_array.iter().any(|t| t["name"].as_str() == Some("test")),
        "legacy inline profile `test` should appear in tools/list, got: {:?}",
        tools_array
    );

    let result = &response["result"];
    let is_error = result["isError"].as_bool().unwrap_or(false);
    assert!(
        !is_error,
        "legacy config tools/call should not be isError, got: {:?}",
        result
    );
    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("hello"),
        "tools/call output should contain `hello`, got: {:?}",
        text
    );
}

// ─── C-PS2: explicit file provider loads from a different YAML file ───

/// C-PS2: When `profile_source: { provider: file, path: <other.yaml> }` is set,
/// Ostia must load bundles + profiles from the OTHER file, not from the
/// `--config` file itself.
#[test]
fn file_provider_explicit_path_loads_from_other_file() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");

    // The "external" YAML has the real profile. Use `write_mcp_config` to
    // get today's-shape inline-bundles-and-profiles file with a profile named `test`.
    let external = mcp_common::write_mcp_config(workspace.path().to_str().unwrap(), &[]);

    // The bootstrap YAML has ONLY the profile_source block pointing at the external.
    let bootstrap = mcp_common::write_bootstrap_with_profile_source(&format!(
        "  provider: file\n  path: {}",
        external.path().display()
    ));

    let mut client = mcp_common::McpClient::spawn(bootstrap.path());
    client.handshake();

    // Act
    let tools = client.tools_list();
    let response = client.call_tool("test", json!({ "command": "echo loaded-from-external" }));

    // Assert — the profile from the EXTERNAL file (not the bootstrap) is visible
    let tools_array = tools["result"]["tools"]
        .as_array()
        .expect("tools/list returns tools array");
    assert!(
        tools_array.iter().any(|t| t["name"].as_str() == Some("test")),
        "profile `test` from external file should appear in tools/list, got: {:?}",
        tools_array
    );

    let result = &response["result"];
    let is_error = result["isError"].as_bool().unwrap_or(false);
    assert!(
        !is_error,
        "tools/call against external-file-sourced profile should not be isError, got: {:?}",
        result
    );
    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("loaded-from-external"),
        "external-file-sourced profile should execute the command, got: {:?}",
        text
    );
}

// ─── C-PS3: source data wins over inline `profiles:` in bootstrap ───

/// C-PS3: When the bootstrap has BOTH a `profile_source:` block AND a top-level
/// inline `profiles:` block (named `decoy`), the source's data must win.
/// `decoy` must NOT appear in tools/list.
#[test]
fn file_provider_explicit_ignores_inline_profiles_in_bootstrap() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");

    // External file has profile `test` (it'll be the "real" one).
    let external = mcp_common::write_mcp_config(workspace.path().to_str().unwrap(), &[]);

    // Bootstrap has profile_source pointing at external AND an inline `decoy` profile.
    let bootstrap = mcp_common::write_bootstrap_with_inline_decoy(
        &format!("  provider: file\n  path: {}", external.path().display()),
        workspace.path().to_str().unwrap(),
    );

    let mut client = mcp_common::McpClient::spawn(bootstrap.path());
    client.handshake();

    // Act
    let tools = client.tools_list();

    // Assert — `test` from external is present; `decoy` from inline bootstrap is absent
    let tools_array = tools["result"]["tools"]
        .as_array()
        .expect("tools/list returns tools array");
    let names: Vec<&str> = tools_array
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(
        names.contains(&"test"),
        "source-provided profile `test` should appear, got: {:?}",
        names
    );
    assert!(
        !names.contains(&"decoy"),
        "inline bootstrap profile `decoy` should be ignored when profile_source is set, got: {:?}",
        names
    );
}

// ─── C-PS13: unknown provider name is a config error ───

/// C-PS13: A `profile_source: { provider: ftp, ... }` value (anything not in
/// {file, http, postgres}) must cause `ostia serve` to exit non-zero before
/// binding any listener. stderr must mention "ftp", "unknown", or "provider".
#[test]
fn unknown_provider_name_blocks_startup() {
    // Arrange
    let bootstrap = mcp_common::write_bootstrap_with_profile_source(
        "  provider: ftp\n  url: \"ftp://nope/profiles\"",
    );

    // Act — try to spawn; expect it to exit on startup
    let outcome = mcp_common::spawn_or_capture_startup_failure(bootstrap.path(), &[]);

    // Assert — process exited non-zero with stderr mentioning the bad provider
    match outcome {
        mcp_common::StdioStartupOutcome::ExitedEarly { status, stderr } => {
            assert!(
                !status.success(),
                "expected non-zero exit for unknown provider, got status: {:?}",
                status
            );
            let lower = stderr.to_lowercase();
            assert!(
                lower.contains("ftp") || lower.contains("unknown") || lower.contains("provider"),
                "stderr should mention ftp/unknown/provider, got: {:?}",
                stderr
            );
        }
        mcp_common::StdioStartupOutcome::Ready(_) => {
            panic!(
                "expected ostia serve to exit when profile_source.provider is `ftp`, but the server stayed alive"
            );
        }
    }
}
