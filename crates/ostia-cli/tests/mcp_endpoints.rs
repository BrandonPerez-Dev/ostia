/// Integration tests for endpoint routing (V10, Slice 3).
///
/// Tests that HTTP endpoints scope tools/list and tools/call to specific
/// profile subsets via /mcp/{name} routing.

mod mcp_common;

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

// ─── HTTP helpers (adapted for endpoint paths) ───

fn available_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn spawn_http_server(config_path: &str, port: u16) -> Child {
    let child = Command::new(mcp_common::ostia_bin())
        .args([
            "serve",
            "--config",
            config_path,
            "--transport",
            "http",
            "--port",
            &port.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ostia serve --transport http");

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if TcpStream::connect(format!("127.0.0.1:{}", port)).is_ok() {
            return child;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("HTTP server did not start within 5 seconds on port {}", port);
}

/// Send a JSON-RPC request to a specific endpoint path.
fn http_jsonrpc_path(port: u16, path: &str, request: &Value) -> Value {
    let body = serde_json::to_string(request).unwrap();
    let mut stream =
        TcpStream::connect(format!("127.0.0.1:{}", port)).expect("connect to MCP HTTP server");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok();

    let http_request = format!(
        "POST {} HTTP/1.1\r\n\
         Host: 127.0.0.1:{}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Accept: application/json\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        path,
        port,
        body.len(),
        body
    );
    stream
        .write_all(http_request.as_bytes())
        .expect("send HTTP request");

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("read HTTP response");

    let body_start = response
        .find("\r\n\r\n")
        .expect("HTTP response should have header/body separator")
        + 4;
    let json_body = &response[body_start..];

    serde_json::from_str(json_body.trim()).expect("parse JSON-RPC response from HTTP body")
}

fn http_handshake_path(port: u16, path: &str) -> Value {
    let response = http_jsonrpc_path(
        port,
        path,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "ostia-endpoint-test", "version": "0.1.0" }
            }
        }),
    );

    let _ = http_jsonrpc_path(
        port,
        path,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    );

    response
}

fn http_tools_list(port: u16, path: &str) -> Value {
    http_jsonrpc_path(
        port,
        path,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/list",
            "params": {}
        }),
    )
}

fn http_call_tool(port: u16, path: &str, name: &str, arguments: Value) -> Value {
    http_jsonrpc_path(
        port,
        path,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments
            }
        }),
    )
}

/// Contract 36: Configured endpoint returns profile subset
///
/// Setup: Config with 3 profiles (alpha, beta, gamma), endpoints: { group: [alpha, beta] }
/// Action: tools/list on /mcp/group
/// Expected: Exactly 2 tools named alpha and beta. No gamma.
#[test]
fn mcp_endpoint_returns_profile_subset() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_endpoint_config(workspace.path().to_str().unwrap());
    let port = available_port();
    let mut child = spawn_http_server(config.path().to_str().unwrap(), port);
    http_handshake_path(port, "/mcp/group");

    // Act
    let response = http_tools_list(port, "/mcp/group");

    // Assert — exactly 2 tools: alpha and beta
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools should be an array");
    let tool_names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();

    assert_eq!(
        tools.len(),
        2,
        "endpoint 'group' should return exactly 2 tools, got: {:?}",
        tool_names
    );
    assert!(
        tool_names.contains(&"alpha"),
        "endpoint 'group' should include 'alpha', got: {:?}",
        tool_names
    );
    assert!(
        tool_names.contains(&"beta"),
        "endpoint 'group' should include 'beta', got: {:?}",
        tool_names
    );
    assert!(
        !tool_names.contains(&"gamma"),
        "endpoint 'group' should NOT include 'gamma', got: {:?}",
        tool_names
    );

    // Cleanup
    child.kill().ok();
    child.wait().ok();
}

/// Contract 37: Single profile name as endpoint
///
/// Setup: Same config (no endpoint named "gamma", but profile exists)
/// Action: tools/list on /mcp/gamma
/// Expected: Exactly 1 tool named gamma
#[test]
fn mcp_endpoint_single_profile_fallback() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_endpoint_config(workspace.path().to_str().unwrap());
    let port = available_port();
    let mut child = spawn_http_server(config.path().to_str().unwrap(), port);
    http_handshake_path(port, "/mcp/gamma");

    // Act
    let response = http_tools_list(port, "/mcp/gamma");

    // Assert — exactly 1 tool: gamma
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools should be an array");
    let tool_names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();

    assert_eq!(
        tools.len(),
        1,
        "endpoint /mcp/gamma (profile fallback) should return exactly 1 tool, got: {:?}",
        tool_names
    );
    assert_eq!(
        tool_names[0], "gamma",
        "single-profile endpoint should return tool named 'gamma', got: {:?}",
        tool_names
    );

    // Cleanup
    child.kill().ok();
    child.wait().ok();
}

/// Contract 38: Default /mcp returns all profiles
///
/// Setup: Same config with 3 profiles
/// Action: tools/list on /mcp
/// Expected: 3 tools (alpha, beta, gamma)
#[test]
fn mcp_endpoint_default_returns_all_profiles() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_endpoint_config(workspace.path().to_str().unwrap());
    let port = available_port();
    let mut child = spawn_http_server(config.path().to_str().unwrap(), port);
    http_handshake_path(port, "/mcp");

    // Act
    let response = http_tools_list(port, "/mcp");

    // Assert — all 3 tools
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools should be an array");
    let tool_names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();

    assert_eq!(
        tools.len(),
        3,
        "default /mcp should return all 3 profile tools, got: {:?}",
        tool_names
    );
    assert!(tool_names.contains(&"alpha"), "should include alpha, got: {:?}", tool_names);
    assert!(tool_names.contains(&"beta"), "should include beta, got: {:?}", tool_names);
    assert!(tool_names.contains(&"gamma"), "should include gamma, got: {:?}", tool_names);

    // Cleanup
    child.kill().ok();
    child.wait().ok();
}

/// Contract 39: Invalid endpoint returns error
///
/// Setup: Same config
/// Action: tools/list on /mcp/nonexistent
/// Expected: Error response (JSON-RPC error)
#[test]
fn mcp_endpoint_invalid_returns_error() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_endpoint_config(workspace.path().to_str().unwrap());
    let port = available_port();
    let mut child = spawn_http_server(config.path().to_str().unwrap(), port);

    // Act — hit a nonexistent endpoint
    let response = http_jsonrpc_path(
        port,
        "/mcp/nonexistent",
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {}
        }),
    );

    // Assert — error response
    assert!(
        response.get("error").is_some(),
        "nonexistent endpoint should return JSON-RPC error, got: {:?}",
        response
    );

    // Cleanup
    child.kill().ok();
    child.wait().ok();
}

/// Contract 40: Execution scoped to endpoint
///
/// Setup: Same config, HTTP server
/// Action: tools/call on /mcp/group with name: "gamma" → error.
///         tools/call on /mcp/group with name: "alpha", arguments: { command: "echo works" } → succeeds.
/// Expected: gamma call returns error (not available on this endpoint).
///           alpha call returns "works".
#[test]
fn mcp_endpoint_execution_scoped() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_endpoint_config(workspace.path().to_str().unwrap());
    let port = available_port();
    let mut child = spawn_http_server(config.path().to_str().unwrap(), port);
    http_handshake_path(port, "/mcp/group");

    // Act 1 — try to call gamma on the group endpoint (should fail)
    let gamma_response = http_call_tool(
        port,
        "/mcp/group",
        "gamma",
        json!({ "command": "echo nope" }),
    );

    // Assert 1 — gamma is not available on this endpoint
    let gamma_result = &gamma_response["result"];
    assert_eq!(
        gamma_result["isError"], true,
        "calling 'gamma' on /mcp/group should return isError: true, got: {:?}",
        gamma_result
    );

    // Act 2 — call alpha on the group endpoint (should succeed)
    let alpha_response = http_call_tool(
        port,
        "/mcp/group",
        "alpha",
        json!({ "command": "echo works" }),
    );

    // Assert 2 — alpha executes and returns output
    let alpha_result = &alpha_response["result"];
    let text = mcp_common::get_content_text(alpha_result);
    assert!(
        text.contains("works"),
        "calling 'alpha' on /mcp/group should return output containing 'works', got: {:?}",
        text
    );
    assert!(
        alpha_result.get("isError").is_none() || alpha_result["isError"] == json!(false),
        "alpha call should not return isError, got: {:?}",
        alpha_result
    );

    // Cleanup
    child.kill().ok();
    child.wait().ok();
}
