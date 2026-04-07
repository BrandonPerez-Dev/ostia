//! Integration tests: MCP error handling (V7, Slice 2).
//!
//! Validates that the MCP server returns correct errors for denied commands,
//! invalid profiles, missing arguments, non-zero exit codes, and auth failures.

mod mcp_common;

use serde_json::json;

/// Contract 5: Denied command returns isError
/// When a client calls a profile tool with a command not in the profile,
/// Then the response has isError: true and mentions the denial.
#[test]
fn mcp_denied_command_returns_error() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_mcp_config(workspace.path().to_str().unwrap(), &[]);
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    // Act
    let response = client.call_tool(
        "test",
        json!({"command": "curl http://evil.com"}),
    );

    // Assert
    let result = &response["result"];
    assert_eq!(
        result["isError"], true,
        "denied command should set isError, got: {:?}",
        result
    );

    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("denied")
            || text.contains("not allowed")
            || text.contains("not whitelisted"),
        "should mention denial, got: {:?}",
        text
    );
}

/// Contract 6: Unknown tool name returns isError
/// When a client calls a tool that doesn't match any profile,
/// Then the response has isError: true and mentions the unknown tool.
#[test]
fn mcp_invalid_profile_returns_error() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_mcp_config(workspace.path().to_str().unwrap(), &[]);
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    // Act
    let response = client.call_tool(
        "nonexistent",
        json!({"command": "echo hello"}),
    );

    // Assert
    let result = &response["result"];
    assert_eq!(
        result["isError"], true,
        "unknown tool should set isError, got: {:?}",
        result
    );

    let text = mcp_common::get_content_text(result);
    assert!(
        text.to_lowercase().contains("unknown")
            || text.to_lowercase().contains("not found"),
        "should mention unknown tool, got: {:?}",
        text
    );
}

/// Contract 7: Missing required argument returns error
/// When a client calls a profile tool without the command argument,
/// Then the response indicates the error (isError or JSON-RPC error).
#[test]
fn mcp_missing_required_argument() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_mcp_config(workspace.path().to_str().unwrap(), &[]);
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    // Act — call profile tool with no command argument
    let response = client.call_tool("test", json!({}));

    // Assert — either isError in result or JSON-RPC error object
    let has_error = response["result"]["isError"] == true || response.get("error").is_some();
    assert!(
        has_error,
        "missing required argument should produce an error, got: {:?}",
        response
    );
}

/// Contract 8: Non-zero exit code is not an MCP error
/// When a command runs but exits non-zero,
/// Then isError is false (the tool worked correctly), and the content
/// includes the exit code so the agent can distinguish "ran but failed"
/// from "denied".
#[test]
fn mcp_nonzero_exit_is_not_mcp_error() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_mcp_config(workspace.path().to_str().unwrap(), &[]);
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    // Act
    let response = client.call_tool(
        "test",
        json!({"command": "exit 42"}),
    );

    // Assert
    let result = &response["result"];
    assert!(
        result["isError"].is_null() || result["isError"] == false,
        "non-zero exit should NOT be isError (tool worked, command failed), got: {:?}",
        result
    );

    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("42"),
        "content should include exit code 42, got: {:?}",
        text
    );
}

/// Contract 9: Auth check failure returns isError
/// When a profile has an auth check that fails (check command exits non-zero),
/// Then calling the profile tool returns isError: true with auth failure details.
#[test]
fn mcp_auth_failure_returns_error() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_auth_fail_config(workspace.path().to_str().unwrap());
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    // Act
    let response = client.call_tool(
        "auth-test",
        json!({"command": "echo hello"}),
    );

    // Assert
    let result = &response["result"];
    assert_eq!(
        result["isError"], true,
        "auth failure should set isError, got: {:?}",
        result
    );

    let text = mcp_common::get_content_text(result);
    assert!(
        text.to_lowercase().contains("auth") || text.to_lowercase().contains("inactive"),
        "should mention auth failure, got: {:?}",
        text
    );
}
