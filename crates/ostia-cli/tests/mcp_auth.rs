//! Integration tests: MCP profile auth binding (V7, Slice 4).
//!
//! Validates that the MCP server correctly handles profile authentication
//! in both open mode (raw profile names) and token mode (AES-GCM encrypted
//! profile tokens). Covers token validation, rejection of invalid tokens,
//! per-request profile switching, and HTTP transport with tokens.

mod mcp_common;

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

// ─── HTTP helpers (duplicated from mcp_http.rs for independence) ───

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
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("HTTP server did not start within 5 seconds on port {}", port);
}

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

    let body_start = response
        .find("\r\n\r\n")
        .expect("HTTP response should have header/body separator")
        + 4;
    let json_body = &response[body_start..];
    serde_json::from_str(json_body.trim()).expect("parse JSON-RPC response from HTTP body")
}

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
                "clientInfo": { "name": "ostia-auth-test", "version": "0.1.0" }
            }
        }),
    );

    let _ = http_jsonrpc(
        port,
        &json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }),
    );

    response
}

// ─── Slice 4a: Auth mode gating ───

/// Contract 18: Explicit open mode accepts raw profile names
/// When the config has auth.mode: open explicitly set,
/// Then raw profile names continue to work as before.
#[test]
fn mcp_open_mode_accepts_raw_profile() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_open_mode_config(workspace.path().to_str().unwrap());
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    // Act
    let response = client.call_tool(
        "run_command",
        json!({"profile": "test", "command": "echo open-works"}),
    );

    // Assert
    let result = &response["result"];
    assert!(
        result["isError"].is_null() || result["isError"] == false,
        "open mode should accept raw profile names, got: {:?}",
        result
    );

    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("open-works"),
        "output should contain 'open-works', got: {:?}",
        text
    );
}

// ─── Slice 4b: AES-GCM token flow ───

/// Contract 13: Token mode — valid encrypted token executes command
/// When a client sends a validly encrypted profile token in token mode,
/// Then the server decrypts it, resolves the profile, and executes the command.
#[test]
fn mcp_token_mode_valid_token_executes() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let key = mcp_common::generate_auth_key();
    let config =
        mcp_common::write_token_mode_config(workspace.path().to_str().unwrap(), &key);
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    let token = mcp_common::encrypt_profile(&key, "test");

    // Act
    let response = client.call_tool(
        "run_command",
        json!({"profile": token, "command": "echo token-works"}),
    );

    // Assert
    let result = &response["result"];
    assert!(
        result["isError"].is_null() || result["isError"] == false,
        "valid token should not produce an error, got: {:?}",
        result
    );

    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("token-works"),
        "output should contain 'token-works', got: {:?}",
        text
    );
}

/// Contract 14: Token mode — invalid token is rejected
/// When a client sends a non-decryptable string as profile in token mode,
/// Then the server returns isError with an auth-related message.
#[test]
fn mcp_token_mode_invalid_token_rejected() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let key = mcp_common::generate_auth_key();
    let config =
        mcp_common::write_token_mode_config(workspace.path().to_str().unwrap(), &key);
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    // Act — send a garbage string that can't be decrypted
    let response = client.call_tool(
        "run_command",
        json!({"profile": "not-a-valid-token", "command": "echo hello"}),
    );

    // Assert
    let result = &response["result"];
    assert!(
        result["isError"] == true,
        "invalid token should return isError: true, got: {:?}",
        result
    );

    let text = mcp_common::get_content_text(result);
    let text_lower = text.to_lowercase();
    assert!(
        text_lower.contains("auth")
            || text_lower.contains("token")
            || text_lower.contains("invalid")
            || text_lower.contains("decrypt"),
        "error should mention auth/token/invalid/decrypt, got: {:?}",
        text
    );
}

/// Contract 15: Token mode — token decrypts to non-existent profile
/// When a client sends a validly encrypted token for a profile that doesn't exist,
/// Then the server returns isError mentioning the profile is not found.
#[test]
fn mcp_token_mode_nonexistent_profile() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let key = mcp_common::generate_auth_key();
    let config =
        mcp_common::write_token_mode_config(workspace.path().to_str().unwrap(), &key);
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    let token = mcp_common::encrypt_profile(&key, "nonexistent");

    // Act
    let response = client.call_tool(
        "run_command",
        json!({"profile": token, "command": "echo hello"}),
    );

    // Assert
    let result = &response["result"];
    assert!(
        result["isError"] == true,
        "token for non-existent profile should return isError: true, got: {:?}",
        result
    );

    let text = mcp_common::get_content_text(result);
    let text_lower = text.to_lowercase();
    assert!(
        text_lower.contains("profile")
            || text_lower.contains("not found")
            || text_lower.contains("unknown"),
        "error should mention profile/not found/unknown, got: {:?}",
        text
    );
}

/// Contract 17: Token mode — list_commands with valid token
/// When a client calls list_commands with a valid encrypted token,
/// Then the server returns the binaries for the decrypted profile.
#[test]
fn mcp_token_mode_list_commands() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let key = mcp_common::generate_auth_key();
    let config =
        mcp_common::write_token_mode_config(workspace.path().to_str().unwrap(), &key);
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    let token = mcp_common::encrypt_profile(&key, "test");

    // Act
    let response = client.call_tool("list_commands", json!({"profile": token}));

    // Assert
    let result = &response["result"];
    assert!(
        result["isError"].is_null() || result["isError"] == false,
        "list_commands with valid token should not error, got: {:?}",
        result
    );

    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("echo"),
        "should list 'echo' binary, got: {:?}",
        text
    );
}

// ─── Slice 4c: Per-request profile switching ───

/// Contract 16: Token mode — per-request profile switching
/// When a client sends different encrypted tokens on consecutive tool calls,
/// Then each call resolves to the correct profile.
#[test]
fn mcp_token_mode_per_request_profile_switching() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let key = mcp_common::generate_auth_key();
    let config =
        mcp_common::write_token_mode_multi_config(workspace.path().to_str().unwrap(), &key);
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    let token_alpha = mcp_common::encrypt_profile(&key, "alpha");
    let token_beta = mcp_common::encrypt_profile(&key, "beta");

    // Act — two consecutive calls with different tokens
    let response_a = client.call_tool(
        "run_command",
        json!({"profile": token_alpha, "command": "echo alpha-ok"}),
    );
    let response_b = client.call_tool(
        "run_command",
        json!({"profile": token_beta, "command": "echo beta-ok"}),
    );

    // Assert
    let text_a = mcp_common::get_content_text(&response_a["result"]);
    let text_b = mcp_common::get_content_text(&response_b["result"]);

    assert!(
        text_a.contains("alpha-ok"),
        "first call should use alpha profile, got: {:?}",
        text_a
    );
    assert!(
        text_b.contains("beta-ok"),
        "second call should use beta profile, got: {:?}",
        text_b
    );
}

/// Contract 19: Token mode works over HTTP transport
/// When a client sends an encrypted token via HTTP POST,
/// Then the server decrypts and executes correctly.
#[test]
fn mcp_token_mode_http_executes() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let key = mcp_common::generate_auth_key();
    let config =
        mcp_common::write_token_mode_config(workspace.path().to_str().unwrap(), &key);
    let port = available_port();
    let mut child = spawn_http_server(config.path().to_str().unwrap(), port);
    http_handshake(port);

    let token = mcp_common::encrypt_profile(&key, "test");

    // Act
    let response = http_jsonrpc(
        port,
        &json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "tools/call",
            "params": {
                "name": "run_command",
                "arguments": {"profile": token, "command": "echo http-token"}
            }
        }),
    );

    // Assert
    let result = &response["result"];
    assert!(
        result["isError"].is_null() || result["isError"] == false,
        "token mode over HTTP should work, got: {:?}",
        result
    );

    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("http-token"),
        "output should contain 'http-token', got: {:?}",
        text
    );

    // Cleanup
    child.kill().ok();
    child.wait().ok();
}
