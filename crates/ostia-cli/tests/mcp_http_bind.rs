//! Integration tests: HTTP bind address and port configurability (V9, Slice 1).
//!
//! Validates that the MCP HTTP server respects --host, --port, and OSTIA_PORT
//! for configuring where it listens.

mod mcp_common;

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn available_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Spawn an HTTP MCP server with explicit host and port flags.
fn spawn_http_server_with_host(
    config_path: &str,
    host: &str,
    port: u16,
) -> Child {
    Command::new(mcp_common::ostia_bin())
        .args([
            "serve",
            "--config",
            config_path,
            "--transport",
            "http",
            "--host",
            host,
            "--port",
            &port.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ostia serve --transport http")
}

/// Spawn an HTTP MCP server with only --port (no --host).
fn spawn_http_server_default_host(config_path: &str, port: u16) -> Child {
    Command::new(mcp_common::ostia_bin())
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
        .expect("spawn ostia serve --transport http")
}

/// Spawn an HTTP MCP server using OSTIA_PORT env var, no --port flag.
fn spawn_http_server_env_port(config_path: &str, port: u16) -> Child {
    Command::new(mcp_common::ostia_bin())
        .args([
            "serve",
            "--config",
            config_path,
            "--transport",
            "http",
        ])
        .env("OSTIA_PORT", port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ostia serve --transport http with OSTIA_PORT")
}

/// Spawn an HTTP MCP server with both --port flag and OSTIA_PORT env var.
fn spawn_http_server_port_override(
    config_path: &str,
    flag_port: u16,
    env_port: u16,
) -> Child {
    Command::new(mcp_common::ostia_bin())
        .args([
            "serve",
            "--config",
            config_path,
            "--transport",
            "http",
            "--port",
            &flag_port.to_string(),
        ])
        .env("OSTIA_PORT", env_port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ostia serve with --port and OSTIA_PORT")
}

/// Wait for a server to accept connections on the given host:port.
fn wait_for_server(host: &str, port: u16) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if TcpStream::connect(format!("{}:{}", host, port)).is_ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Send a JSON-RPC request over HTTP and return the parsed JSON body.
fn http_jsonrpc(host: &str, port: u16, request: &Value) -> Value {
    let body = serde_json::to_string(request).unwrap();
    let mut stream =
        TcpStream::connect(format!("{}:{}", host, port)).expect("connect to MCP HTTP server");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok();

    let http_request = format!(
        "POST /mcp HTTP/1.1\r\n\
         Host: {}:{}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Accept: application/json\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        host,
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
    serde_json::from_str(json_body.trim()).expect("parse JSON-RPC response")
}

/// Contract 21: --host flag changes bind address
/// When ostia serve is started with --host 0.0.0.0,
/// Then the server accepts connections on all interfaces.
#[test]
fn mcp_http_host_flag_changes_bind_address() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_mcp_config(workspace.path().to_str().unwrap(), &[]);
    let port = available_port();
    let mut child = spawn_http_server_with_host(
        config.path().to_str().unwrap(),
        "0.0.0.0",
        port,
    );

    // Act — connect via 127.0.0.1 (should work when bound to 0.0.0.0)
    assert!(
        wait_for_server("127.0.0.1", port),
        "server should accept connections when bound to 0.0.0.0"
    );

    let response = http_jsonrpc(
        "127.0.0.1",
        port,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "bind-test", "version": "0.1.0" }
            }
        }),
    );

    // Assert
    let server_name = response["result"]["serverInfo"]["name"]
        .as_str()
        .unwrap_or("");
    assert!(
        server_name.to_lowercase().contains("ostia"),
        "should get valid MCP response, got: {:?}",
        response
    );

    // Cleanup
    child.kill().ok();
    child.wait().ok();
}

/// Contract 22: Default host is 127.0.0.1
/// When ostia serve is started without --host,
/// Then the server binds to localhost only.
#[test]
fn mcp_http_default_host_is_localhost() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_mcp_config(workspace.path().to_str().unwrap(), &[]);
    let port = available_port();
    let mut child = spawn_http_server_default_host(
        config.path().to_str().unwrap(),
        port,
    );

    // Act — connect via 127.0.0.1
    assert!(
        wait_for_server("127.0.0.1", port),
        "server should accept connections on localhost by default"
    );

    let response = http_jsonrpc(
        "127.0.0.1",
        port,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "default-host-test", "version": "0.1.0" }
            }
        }),
    );

    // Assert
    let server_name = response["result"]["serverInfo"]["name"]
        .as_str()
        .unwrap_or("");
    assert!(
        server_name.to_lowercase().contains("ostia"),
        "should get valid MCP response on default host, got: {:?}",
        response
    );

    // Cleanup
    child.kill().ok();
    child.wait().ok();
}

/// Contract 23: OSTIA_PORT env var sets port
/// When OSTIA_PORT is set and no --port flag is given,
/// Then the server listens on the env var port.
#[test]
fn mcp_http_env_var_sets_port() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_mcp_config(workspace.path().to_str().unwrap(), &[]);
    let port = available_port();
    let mut child = spawn_http_server_env_port(
        config.path().to_str().unwrap(),
        port,
    );

    // Act
    assert!(
        wait_for_server("127.0.0.1", port),
        "server should listen on OSTIA_PORT ({})",
        port
    );

    let response = http_jsonrpc(
        "127.0.0.1",
        port,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "env-port-test", "version": "0.1.0" }
            }
        }),
    );

    // Assert
    assert!(
        response["result"]["serverInfo"]["name"].is_string(),
        "should get valid MCP response on env var port, got: {:?}",
        response
    );

    // Cleanup
    child.kill().ok();
    child.wait().ok();
}

/// Contract 24: --port flag overrides OSTIA_PORT env var
/// When both --port and OSTIA_PORT are set,
/// Then the server listens on the --port value.
#[test]
fn mcp_http_port_flag_overrides_env_var() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_mcp_config(workspace.path().to_str().unwrap(), &[]);
    let flag_port = available_port();
    let env_port = available_port();

    // Ensure they're different
    assert_ne!(flag_port, env_port, "ports must differ for this test");

    let mut child = spawn_http_server_port_override(
        config.path().to_str().unwrap(),
        flag_port,
        env_port,
    );

    // Act — server should be on flag_port, not env_port
    assert!(
        wait_for_server("127.0.0.1", flag_port),
        "server should listen on --port ({}), not OSTIA_PORT ({})",
        flag_port,
        env_port
    );

    let response = http_jsonrpc(
        "127.0.0.1",
        flag_port,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "override-test", "version": "0.1.0" }
            }
        }),
    );

    // Assert
    assert!(
        response["result"]["serverInfo"]["name"].is_string(),
        "should get valid MCP response on flag port, got: {:?}",
        response
    );

    // Verify env_port is NOT listening
    let env_port_open = TcpStream::connect(format!("127.0.0.1:{}", env_port)).is_ok();
    assert!(
        !env_port_open,
        "server should NOT be listening on OSTIA_PORT ({}) when --port is given",
        env_port
    );

    // Cleanup
    child.kill().ok();
    child.wait().ok();
}
