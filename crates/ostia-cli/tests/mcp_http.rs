//! Integration tests: MCP streamable HTTP transport (V7, Slice 3).
//!
//! Validates that the MCP server works over HTTP: starts, accepts
//! initialize, executes commands, and handles concurrent clients
//! with different profiles.

mod mcp_common;

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Find an available TCP port by binding to port 0.
fn available_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Spawn an HTTP MCP server. Waits up to 5 seconds for it to accept connections.
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

    // Wait for server to accept connections
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if TcpStream::connect(format!("127.0.0.1:{}", port)).is_ok() {
            return child;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("HTTP server did not start within 5 seconds on port {}", port);
}

/// Send a JSON-RPC request over HTTP and return the parsed JSON body.
fn http_jsonrpc(port: u16, request: &Value) -> Value {
    let body = serde_json::to_string(request).unwrap();
    let mut stream =
        TcpStream::connect(format!("127.0.0.1:{}", port)).expect("connect to MCP HTTP server");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok();

    let http_request = format!(
        "POST /mcp HTTP/1.1\r\n\
         Host: 127.0.0.1:{}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Accept: application/json\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
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

    // Extract JSON body after the header/body separator
    let body_start = response
        .find("\r\n\r\n")
        .expect("HTTP response should have header/body separator")
        + 4;
    let json_body = &response[body_start..];

    serde_json::from_str(json_body.trim()).expect("parse JSON-RPC response from HTTP body")
}

/// Complete an HTTP MCP handshake. Returns the initialize response.
fn http_handshake(port: u16) -> Value {
    let response = http_jsonrpc(
        port,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "ostia-http-test", "version": "0.1.0" }
            }
        }),
    );

    // Send initialized notification
    let _ = http_jsonrpc(
        port,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    );

    response
}

/// Contract 10: HTTP server starts and accepts initialize
/// When an MCP client sends initialize over HTTP,
/// Then the server responds with protocolVersion and serverInfo.
#[test]
fn mcp_http_initialize_handshake() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_mcp_config(workspace.path().to_str().unwrap(), &[]);
    let port = available_port();
    let mut child = spawn_http_server(config.path().to_str().unwrap(), port);

    // Act
    let response = http_handshake(port);

    // Assert
    let result = &response["result"];
    assert!(
        result["protocolVersion"].is_string(),
        "should have protocolVersion, got: {:?}",
        result
    );
    let server_name = result["serverInfo"]["name"].as_str().unwrap_or("");
    assert!(
        server_name.to_lowercase().contains("ostia"),
        "serverInfo.name should contain 'ostia', got: {:?}",
        server_name
    );

    // Cleanup
    child.kill().ok();
    child.wait().ok();
}

/// Contract 11: HTTP run_command executes a sandboxed command
/// When a client calls run_command over HTTP,
/// Then the response contains the command output.
#[test]
fn mcp_http_run_command_executes() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_mcp_config(workspace.path().to_str().unwrap(), &[]);
    let port = available_port();
    let mut child = spawn_http_server(config.path().to_str().unwrap(), port);
    http_handshake(port);

    // Act
    let response = http_jsonrpc(
        port,
        &json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "tools/call",
            "params": {
                "name": "run_command",
                "arguments": {"profile": "test", "command": "echo http-works"}
            }
        }),
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
        text.contains("http-works"),
        "output should contain 'http-works', got: {:?}",
        text
    );

    // Cleanup
    child.kill().ok();
    child.wait().ok();
}

/// Contract 12: Concurrent clients with different profiles
/// When two clients use different profiles concurrently over HTTP,
/// Then both get correct responses without blocking each other.
#[test]
fn mcp_http_concurrent_clients_different_profiles() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_multi_profile_config(workspace.path().to_str().unwrap());
    let port = available_port();
    let mut child = spawn_http_server(config.path().to_str().unwrap(), port);

    // Act — two threads, each using a different profile
    let handle_a = thread::spawn(move || {
        http_handshake(port);
        http_jsonrpc(
            port,
            &json!({
                "jsonrpc": "2.0",
                "id": 20,
                "method": "tools/call",
                "params": {
                    "name": "run_command",
                    "arguments": {"profile": "alpha", "command": "echo alpha-ok"}
                }
            }),
        )
    });

    let handle_b = thread::spawn(move || {
        http_handshake(port);
        http_jsonrpc(
            port,
            &json!({
                "jsonrpc": "2.0",
                "id": 21,
                "method": "tools/call",
                "params": {
                    "name": "run_command",
                    "arguments": {"profile": "beta", "command": "echo beta-ok"}
                }
            }),
        )
    });

    let response_a = handle_a.join().expect("client A should complete");
    let response_b = handle_b.join().expect("client B should complete");

    // Assert
    let text_a = mcp_common::get_content_text(&response_a["result"]);
    let text_b = mcp_common::get_content_text(&response_b["result"]);

    assert!(
        text_a.contains("alpha-ok"),
        "client A should get alpha-ok, got: {:?}",
        text_a
    );
    assert!(
        text_b.contains("beta-ok"),
        "client B should get beta-ok, got: {:?}",
        text_b
    );

    // Cleanup
    child.kill().ok();
    child.wait().ok();
}
