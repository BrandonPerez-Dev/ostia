//! Integration tests: HTTP credential provider + user identity (V4).
//!
//! Validates that the `http` provider fetches JSON from a URL, maps response
//! keys via `inject` into sandbox env vars, and that user identity is
//! interpolated into URL templates via `{{ user_id }}`.

mod mcp_common;

use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

/// Start a mock HTTP server that responds with the given JSON body.
///
/// Returns `(port, join_handle)`. The server handles exactly one request,
/// then the join handle returns the raw request string for verification.
fn start_mock_http_server(json_body: &str, status: u16) -> (u16, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
    let port = listener.local_addr().unwrap().port();
    let body = json_body.to_string();

    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept connection");
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).expect("read request");
        let request = String::from_utf8_lossy(&buf[..n]).to_string();

        let response = format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).expect("write response");

        request
    });

    (port, handle)
}

/// V4 Slice 1: HTTP provider fetches JSON and injects keys
/// When a profile has `provider: http` pointing at a mock server,
/// Then the JSON response keys are mapped via `inject` into sandbox env vars.
#[test]
fn http_provider_fetches_json_and_injects() {
    // Arrange
    mcp_common::assert_user_namespaces();

    let (port, _server) = start_mock_http_server(
        r#"{"access_token": "vault-token-abc", "api_key": "key-xyz"}"#,
        200,
    );

    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_credential_config(
        workspace.path().to_str().unwrap(),
        &format!(
            r#"    credentials:
      vault:
        provider: http
        url: "http://127.0.0.1:{port}/secrets"
        inject:
          ACCESS_TOKEN: access_token
          API_KEY: api_key"#
        ),
    );
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    // Act
    let response = client.call_tool("test", json!({"command": "echo $ACCESS_TOKEN:$API_KEY"}));

    // Assert
    let result = &response["result"];
    let text = mcp_common::get_content_text(result);
    assert_eq!(
        text.trim(),
        "vault-token-abc:key-xyz",
        "http provider should inject JSON response keys into sandbox, got: {:?}",
        text
    );
}

/// V4 Slice 1 (error case): HTTP provider with server error blocks execution
/// When the vault server returns 500,
/// Then execution is blocked with an error.
#[test]
fn http_provider_server_error_blocks_execution() {
    // Arrange
    mcp_common::assert_user_namespaces();

    let (port, _server) = start_mock_http_server(r#"{"error": "internal"}"#, 500);

    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_credential_config(
        workspace.path().to_str().unwrap(),
        &format!(
            r#"    credentials:
      vault:
        provider: http
        url: "http://127.0.0.1:{port}/secrets"
        inject:
          TOKEN: access_token"#
        ),
    );
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    // Act
    let response = client.call_tool("test", json!({"command": "echo hello"}));

    // Assert
    let result = &response["result"];
    let is_error = result["isError"].as_bool().unwrap_or(false);
    assert!(
        is_error,
        "HTTP 500 from vault should set isError: true, got: {:?}",
        result
    );
    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("500") || text.contains("server error") || text.contains("failed"),
        "error should mention HTTP failure, got: {:?}",
        text
    );
}

/// V4 Slice 2: User identity template interpolation
/// When a profile has `url: "http://.../ {{ user_id }}"` and the server is
/// started with `--user-id user-42`,
/// Then the URL is interpolated and the mock server receives the request
/// at the correct path.
#[test]
fn http_provider_interpolates_user_identity() {
    // Arrange
    mcp_common::assert_user_namespaces();

    let (port, server_handle) = start_mock_http_server(
        r#"{"token": "user-42-token"}"#,
        200,
    );

    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_credential_config(
        workspace.path().to_str().unwrap(),
        &format!(
            r#"    credentials:
      vault:
        provider: http
        url: "http://127.0.0.1:{port}/secrets/{{{{ user_id }}}}"
        inject:
          USER_TOKEN: token"#
        ),
    );
    let mut client = mcp_common::McpClient::spawn_with_args_and_env(
        config.path(),
        &["--user-id", "user-42"],
        &[],
    );
    client.handshake();

    // Act
    let response = client.call_tool("test", json!({"command": "echo $USER_TOKEN"}));

    // Assert — sandbox sees the injected token
    let result = &response["result"];
    let text = mcp_common::get_content_text(result);
    assert_eq!(
        text.trim(),
        "user-42-token",
        "user identity should be interpolated in URL, got: {:?}",
        text
    );

    // Assert — mock server received request at the correct path
    let request = server_handle.join().expect("mock server thread");
    assert!(
        request.contains("/secrets/user-42"),
        "mock server should receive request at /secrets/user-42, got: {:?}",
        request
    );
}

/// V4 Slice 2 (error case): Unresolved template variable produces error
/// When a URL contains `{{ user_id }}` but no user identity is provided,
/// Then execution is blocked with an error.
#[test]
fn http_provider_unresolved_template_blocks_execution() {
    // Arrange
    mcp_common::assert_user_namespaces();

    // No mock server needed — template resolution should fail before HTTP call
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_credential_config(
        workspace.path().to_str().unwrap(),
        r#"    credentials:
      vault:
        provider: http
        url: "http://127.0.0.1:9999/secrets/{{ user_id }}"
        inject:
          TOKEN: token"#,
    );
    // No --user-id flag, no OSTIA_USER_ID env
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    // Act
    let response = client.call_tool("test", json!({"command": "echo hello"}));

    // Assert
    let result = &response["result"];
    let is_error = result["isError"].as_bool().unwrap_or(false);
    let text = mcp_common::get_content_text(result);
    assert!(
        is_error,
        "unresolved template should set isError: true, got: {:?}",
        result
    );
    assert!(
        text.contains("user_id") || text.contains("template") || text.contains("identity"),
        "error should mention unresolved template, got: {:?}",
        text
    );
}
