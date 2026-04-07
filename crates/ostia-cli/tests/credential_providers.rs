//! Integration tests: Credential provider framework + command provider (V1).
//!
//! Validates that the `credentials:` config block parses correctly, that the
//! `command` provider shells out on the host and injects the result into the
//! sandbox via the `inject` mapping, and that a failed credential fetch
//! blocks execution with an error.

mod mcp_common;

use serde_json::json;

/// V1 Slice 1: Config parsing — credentials block deserializes
/// When a profile has a `credentials:` block with a command provider,
/// Then the config parses without error and the profile resolves.
///
/// NOTE: This is effectively tested through V1 Slice 2 (the integration
/// test that uses the config). Kept as a standalone to validate that bad
/// configs are rejected.
#[test]
fn credentials_config_rejects_unknown_provider() {
    // Arrange — config with unknown provider type
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_credential_config(
        workspace.path().to_str().unwrap(),
        r#"    credentials:
      bad:
        provider: ftp
        command: "echo nope"
        inject:
          TOKEN: value"#,
    );

    // Act — spawn server, handshake, try to use the profile
    let mut client = mcp_common::McpClient::spawn(config.path());
    let response = client.handshake();

    // Assert — server should reject unknown provider during config load
    // or profile resolution. Either the handshake fails, or tools/list
    // returns no tools, or the tool call returns an error.
    // The exact failure mode depends on implementation — what matters is
    // that "ftp" is not silently accepted.
    let tools_response = client.tools_list();
    let tools = tools_response["result"]["tools"].as_array();

    // If the server started and listed tools, calling the tool should error
    if let Some(tools) = tools {
        if tools.iter().any(|t| t["name"].as_str() == Some("test")) {
            let call_response =
                client.call_tool("test", json!({"command": "echo hello"}));
            let result = &call_response["result"];
            let is_error = result["isError"].as_bool().unwrap_or(false);
            let text = mcp_common::get_content_text(result);
            assert!(
                is_error || text.contains("error") || text.contains("unknown provider"),
                "unknown provider 'ftp' should produce an error, got: {:?}",
                text
            );
        }
    }
    // If server refused to start or listed no tools, that's also valid rejection
}

/// V1 Slice 2: Command provider fetch + inject
/// When a profile has `credentials: { gcp: { provider: command, command: "echo test-token", inject: { MY_TOKEN: value } } }`,
/// Then `echo $MY_TOKEN` inside the sandbox returns "test-token".
#[test]
fn command_provider_injects_credential_into_sandbox() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_credential_config(
        workspace.path().to_str().unwrap(),
        r#"    credentials:
      gcp:
        provider: command
        command: "echo test-token"
        inject:
          MY_TOKEN: value"#,
    );
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    // Act
    let response = client.call_tool("test", json!({"command": "echo $MY_TOKEN"}));

    // Assert
    let result = &response["result"];
    let text = mcp_common::get_content_text(result);
    assert_eq!(
        text.trim(),
        "test-token",
        "command provider should inject credential into sandbox, got: {:?}",
        text
    );
}

/// V1 Slice 3: Failed credential fetch blocks execution
/// When a profile has `credentials: { bad-cred: { provider: command, command: "false", inject: { SOME_TOKEN: value } } }`,
/// Then execution is blocked with an error — the sandbox never runs.
#[test]
fn failed_credential_fetch_blocks_execution() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_credential_config(
        workspace.path().to_str().unwrap(),
        r#"    credentials:
      bad-cred:
        provider: command
        command: "false"
        inject:
          SOME_TOKEN: value"#,
    );
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    // Act
    let response = client.call_tool("test", json!({"command": "echo hello"}));

    // Assert — should be an error, sandbox never forked
    let result = &response["result"];
    let is_error = result["isError"].as_bool().unwrap_or(false);
    let text = mcp_common::get_content_text(result);
    assert!(
        is_error,
        "failed credential fetch should set isError: true, got: {:?}",
        result
    );
    assert!(
        text.contains("credential") || text.contains("bad-cred") || text.contains("failed"),
        "error message should mention credential failure, got: {:?}",
        text
    );
}
