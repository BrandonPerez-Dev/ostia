/// Integration tests for per-profile tool dispatch (V10, Slice 2).
///
/// Tests that tools/call routes to the correct profile by tool name,
/// executes commands in that profile's sandbox, and enforces per-profile
/// deny rules.

mod mcp_common;

use serde_json::json;

/// Contract 32: Profile tool executes command
///
/// Setup: Config with profile `permissive` (baseline binaries, workspace set)
/// Action: tools/call with name: "permissive", arguments: { command: "echo hi" }
/// Expected: Content contains "hi", isError absent/false
#[test]
fn mcp_profile_tool_executes_command() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config =
        mcp_common::write_diff_rules_config(workspace.path().to_str().unwrap());
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    // Act — call the "permissive" profile tool with echo
    let response = client.call_tool("permissive", json!({ "command": "echo hi" }));

    // Assert — output contains "hi", no error
    let result = &response["result"];
    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("hi"),
        "permissive profile tool should execute 'echo hi' and return output containing 'hi', got: {:?}",
        text
    );
    assert!(
        result.get("isError").is_none() || result["isError"] == json!(false),
        "permissive profile tool should not return isError, got: {:?}",
        result
    );
}

/// Contract 33: Different profiles enforce different deny rules
///
/// Setup: Config with `permissive` (allows cat) and `restrictive` (denies "cat *").
/// Pre-create test.txt in workspace.
/// Action: tools/call "permissive" with cat → succeeds. tools/call "restrictive"
/// with same cat → denied.
#[test]
fn mcp_profile_tools_enforce_different_deny_rules() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let ws_path = workspace.path().to_str().unwrap();

    // Pre-create test file in workspace
    std::fs::write(workspace.path().join("test.txt"), "secret data\n")
        .expect("write test.txt");

    let config = mcp_common::write_diff_rules_config(ws_path);
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    // Act 1 — permissive profile allows cat
    let permissive_response =
        client.call_tool("permissive", json!({ "command": "cat test.txt" }));

    // Assert 1 — permissive returns file content
    let permissive_result = &permissive_response["result"];
    let permissive_text = mcp_common::get_content_text(permissive_result);
    assert!(
        permissive_text.contains("secret data"),
        "permissive profile should allow 'cat test.txt' and return file content, got: {:?}",
        permissive_text
    );
    assert!(
        permissive_result.get("isError").is_none()
            || permissive_result["isError"] == json!(false),
        "permissive profile should not return isError, got: {:?}",
        permissive_result
    );

    // Act 2 — restrictive profile denies cat
    let restrictive_response =
        client.call_tool("restrictive", json!({ "command": "cat test.txt" }));

    // Assert 2 — restrictive returns error with denial message
    let restrictive_result = &restrictive_response["result"];
    assert_eq!(
        restrictive_result["isError"], true,
        "restrictive profile should deny 'cat test.txt' with isError: true, got: {:?}",
        restrictive_result
    );
}

/// Contract 34: Unknown tool name returns error
///
/// Setup: Any valid config
/// Action: tools/call with name: "nonexistent", arguments: { command: "echo" }
/// Expected: isError: true, mentions unknown tool or profile
#[test]
fn mcp_profile_tool_unknown_name_returns_error() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config =
        mcp_common::write_diff_rules_config(workspace.path().to_str().unwrap());
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    // Act — call a tool name that doesn't match any profile
    let response = client.call_tool("nonexistent", json!({ "command": "echo" }));

    // Assert — isError with message about unknown tool
    let result = &response["result"];
    assert_eq!(
        result["isError"], true,
        "unknown tool name should return isError: true, got: {:?}",
        result
    );
    let text = mcp_common::get_content_text(result);
    assert!(
        text.to_lowercase().contains("unknown") || text.to_lowercase().contains("not found"),
        "error message should mention unknown tool or not found, got: {:?}",
        text
    );
}

/// Contract 35: Missing command argument returns error
///
/// Setup: Any valid config with profile `permissive`
/// Action: tools/call with name: "permissive", arguments: {}
/// Expected: isError: true, mentions missing command
#[test]
fn mcp_profile_tool_missing_command_returns_error() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config =
        mcp_common::write_diff_rules_config(workspace.path().to_str().unwrap());
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    // Act — call permissive profile tool with empty arguments
    let response = client.call_tool("permissive", json!({}));

    // Assert — isError with message about missing command
    let result = &response["result"];
    assert_eq!(
        result["isError"], true,
        "missing command should return isError: true, got: {:?}",
        result
    );
    let text = mcp_common::get_content_text(result);
    assert!(
        text.to_lowercase().contains("command"),
        "error message should mention missing command, got: {:?}",
        text
    );
}
