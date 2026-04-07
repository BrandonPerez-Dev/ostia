//! Integration tests: Env injection into sandbox (V0).
//!
//! Validates that the sandbox receives an explicit env vector (via execve),
//! that parent env vars are NOT inherited, and that baseline env vars
//! (PATH, HOME, TERM) are always present.

mod mcp_common;

use serde_json::json;

/// V0 Slice 1: Explicit env injection
/// When a profile has `env: { TEST_CRED: "injected-secret" }`,
/// Then `echo $TEST_CRED` inside the sandbox returns "injected-secret".
#[test]
fn sandbox_receives_injected_env_vars() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_env_injection_config(
        workspace.path().to_str().unwrap(),
        &[("TEST_CRED", "injected-secret")],
    );
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    // Act
    let response = client.call_tool("test", json!({"command": "echo $TEST_CRED"}));

    // Assert
    let result = &response["result"];
    let text = mcp_common::get_content_text(result);
    assert_eq!(
        text.trim(),
        "injected-secret",
        "sandbox should see injected env var, got: {:?}",
        text
    );
}

/// V0 Slice 2: Parent env isolation
/// When the host process has OSTIA_TEST_LEAK set,
/// Then `echo $OSTIA_TEST_LEAK` inside the sandbox returns empty.
#[test]
fn sandbox_does_not_inherit_parent_env() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_mcp_config(workspace.path().to_str().unwrap(), &[]);
    let mut client = mcp_common::McpClient::spawn_with_env(
        config.path(),
        &[("OSTIA_TEST_LEAK", "should-not-see-this")],
    );
    client.handshake();

    // Act
    let response = client.call_tool("test", json!({"command": "echo $OSTIA_TEST_LEAK"}));

    // Assert
    let result = &response["result"];
    let text = mcp_common::get_content_text(result);
    assert!(
        !text.contains("should-not-see-this"),
        "sandbox should NOT see parent env vars, got: {:?}",
        text
    );
}

/// V0 Slice 3: Baseline env vars
/// When a profile has no custom env,
/// Then the sandbox has PATH=/usr/bin:/bin, HOME=/, TERM=dumb.
#[test]
fn sandbox_has_baseline_env_vars() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_mcp_config(workspace.path().to_str().unwrap(), &[]);
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    // Act
    let response = client.call_tool("test", json!({"command": "echo $PATH:$HOME:$TERM"}));

    // Assert
    let result = &response["result"];
    let text = mcp_common::get_content_text(result);
    assert_eq!(
        text.trim(),
        "/usr/bin:/bin:/:/dumb",
        "sandbox should have baseline env vars (PATH:HOME:TERM), got: {:?}",
        text
    );
}
