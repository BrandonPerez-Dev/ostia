//! Integration tests: MCP stdio server walking skeleton (V7, Slice 1).
//!
//! Validates the MCP initialize handshake, tool listing, command discovery,
//! and sandboxed command execution over stdio transport.

mod mcp_common;

use serde_json::json;

/// Contract 1: Initialize handshake
/// When an MCP client connects via stdio and sends initialize,
/// Then the server responds with protocolVersion, capabilities.tools,
/// and serverInfo.name containing "ostia".
#[test]
fn mcp_initialize_handshake() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_mcp_config(workspace.path().to_str().unwrap(), &[]);

    // Act
    let mut client = mcp_common::McpClient::spawn(config.path());
    let response = client.handshake();

    // Assert
    let result = &response["result"];
    assert!(
        result["protocolVersion"].is_string(),
        "should have protocolVersion, got: {:?}",
        result
    );
    assert!(
        result["capabilities"]["tools"].is_object(),
        "should have capabilities.tools, got: {:?}",
        result
    );
    let server_name = result["serverInfo"]["name"].as_str().unwrap_or("");
    assert!(
        server_name.to_lowercase().contains("ostia"),
        "serverInfo.name should contain 'ostia', got: {:?}",
        server_name
    );
}

/// Contract 2: tools/list returns run_command and list_commands
/// When a client sends tools/list after initialization,
/// Then the response contains run_command (requires command + profile)
/// and list_commands (requires profile).
#[test]
fn mcp_tools_list_returns_expected_tools() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_mcp_config(workspace.path().to_str().unwrap(), &[]);
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    // Act
    let response = client.tools_list();

    // Assert
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools should be an array");

    let tool_names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();

    assert!(
        tool_names.contains(&"run_command"),
        "should have run_command tool, got: {:?}",
        tool_names
    );
    assert!(
        tool_names.contains(&"list_commands"),
        "should have list_commands tool, got: {:?}",
        tool_names
    );

    // Verify run_command requires command and profile
    let run_cmd = tools.iter().find(|t| t["name"] == "run_command").unwrap();
    let required: Vec<&str> = run_cmd["inputSchema"]["required"]
        .as_array()
        .expect("run_command should have required fields")
        .iter()
        .filter_map(|r| r.as_str())
        .collect();
    assert!(
        required.contains(&"command"),
        "run_command should require 'command', got: {:?}",
        required
    );
    assert!(
        required.contains(&"profile"),
        "run_command should require 'profile', got: {:?}",
        required
    );

    // Verify list_commands requires profile
    let list_cmd = tools
        .iter()
        .find(|t| t["name"] == "list_commands")
        .unwrap();
    let required: Vec<&str> = list_cmd["inputSchema"]["required"]
        .as_array()
        .expect("list_commands should have required fields")
        .iter()
        .filter_map(|r| r.as_str())
        .collect();
    assert!(
        required.contains(&"profile"),
        "list_commands should require 'profile', got: {:?}",
        required
    );
}

/// Contract 3: list_commands returns available binaries for profile
/// When a client calls list_commands with a valid profile,
/// Then the response lists the allowed binaries from that profile's config.
#[test]
fn mcp_list_commands_returns_binaries() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_mcp_config(workspace.path().to_str().unwrap(), &[]);
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    // Act
    let response = client.call_tool("list_commands", json!({"profile": "test"}));

    // Assert
    let result = &response["result"];
    assert!(
        result["isError"].is_null() || result["isError"] == false,
        "list_commands should not be an error, got: {:?}",
        result
    );

    let text = mcp_common::get_content_text(result);
    for binary in &["sh", "bash", "echo", "cat", "ls"] {
        assert!(
            text.contains(binary),
            "list_commands should include '{}', got: {:?}",
            binary, text
        );
    }
}

/// Contract 4: run_command executes a sandboxed command
/// When a client calls run_command with a valid profile and command,
/// Then the response contains the command output and exit code 0.
#[test]
fn mcp_run_command_executes() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_mcp_config(workspace.path().to_str().unwrap(), &[]);
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    // Act
    let response = client.call_tool(
        "run_command",
        json!({"profile": "test", "command": "echo hello"}),
    );

    // Assert
    let result = &response["result"];
    assert!(
        result["isError"].is_null() || result["isError"] == false,
        "run_command should not be an error, got: {:?}",
        result
    );

    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("hello"),
        "output should contain 'hello', got: {:?}",
        text
    );
}
