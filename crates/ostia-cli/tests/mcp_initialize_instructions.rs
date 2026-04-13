//! Integration tests for the MCP `initialize.instructions` field (issue #6).
//!
//! Contracts C-C1b, C-C1c, and C-C39b from `spec/mcp-server.md`.

mod mcp_common;

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Contract C-C1b: `initialize` returns dynamic `instructions` listing every profile.
///
/// Given a stdio server with two profiles (alpha, beta),
/// When the client sends `initialize`,
/// Then the response includes a non-empty `instructions` string that mentions
/// "ostia" (static preamble) and both profile names (dynamic listing).
#[test]
fn mcp_initialize_instructions_include_profile_list() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_multi_profile_config(workspace.path().to_str().unwrap());

    // Act
    let mut client = mcp_common::McpClient::spawn(config.path());
    let response = client.handshake();

    // Assert — instructions field is present and non-empty
    let instructions = response["result"]["instructions"]
        .as_str()
        .expect("initialize result should include a string `instructions` field");
    assert!(
        !instructions.is_empty(),
        "instructions should not be empty"
    );

    // Static preamble identifies the server
    assert!(
        instructions.to_lowercase().contains("ostia"),
        "instructions should identify the server as ostia, got: {:?}",
        instructions
    );

    // Dynamic profile list includes both configured profiles
    assert!(
        instructions.contains("alpha"),
        "instructions should list profile 'alpha', got: {:?}",
        instructions
    );
    assert!(
        instructions.contains("beta"),
        "instructions should list profile 'beta', got: {:?}",
        instructions
    );
}

/// Contract C-C1c: Single-profile config renders cleanly.
///
/// Given a stdio server with one profile ("test"),
/// When the client sends `initialize`,
/// Then the instructions are non-empty, mention the profile name, and do NOT
/// fall through to the empty-list branch.
#[test]
fn mcp_initialize_instructions_single_profile_renders_cleanly() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_mcp_config(workspace.path().to_str().unwrap(), &[]);

    // Act
    let mut client = mcp_common::McpClient::spawn(config.path());
    let response = client.handshake();

    // Assert
    let instructions = response["result"]["instructions"]
        .as_str()
        .expect("initialize result should include a string `instructions` field");
    assert!(
        !instructions.is_empty(),
        "instructions should not be empty"
    );
    assert!(
        instructions.contains("test"),
        "instructions should list profile 'test', got: {:?}",
        instructions
    );
    assert!(
        !instructions.contains("Available profiles (0)"),
        "single-profile config should not take the empty-list branch, got: {:?}",
        instructions
    );
}

// ─── HTTP helpers (adapted from mcp_endpoints.rs) ───

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

fn http_jsonrpc_path(port: u16, path: &str, request: &Value) -> Value {
    let body = serde_json::to_string(request).unwrap();
    let mut stream =
        TcpStream::connect(format!("127.0.0.1:{}", port)).expect("connect to MCP HTTP server");
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();

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

/// Contract C-C39b: `initialize` instructions are scoped on HTTP endpoint.
///
/// Given three profiles (alpha, beta, gamma) with endpoints: {group: [alpha, beta]},
/// When the client POSTs `initialize` to `/mcp/group`,
/// Then instructions list alpha and beta but NOT gamma — the endpoint filter
/// that scopes `tools/list` also scopes the initialize profile list.
#[test]
fn mcp_initialize_instructions_are_endpoint_scoped() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_endpoint_config(workspace.path().to_str().unwrap());
    let port = available_port();
    let mut child = spawn_http_server(config.path().to_str().unwrap(), port);

    // Act
    let response = http_jsonrpc_path(
        port,
        "/mcp/group",
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "ostia-endpoint-instructions-test", "version": "0.1.0" }
            }
        }),
    );

    // Assert
    let instructions = response["result"]["instructions"]
        .as_str()
        .expect("initialize result should include a string `instructions` field");
    assert!(
        instructions.contains("alpha"),
        "/mcp/group instructions should list 'alpha', got: {:?}",
        instructions
    );
    assert!(
        instructions.contains("beta"),
        "/mcp/group instructions should list 'beta', got: {:?}",
        instructions
    );
    assert!(
        !instructions.contains("gamma"),
        "/mcp/group instructions should NOT list 'gamma' (out of scope), got: {:?}",
        instructions
    );

    // Cleanup
    child.kill().ok();
    child.wait().ok();
}
