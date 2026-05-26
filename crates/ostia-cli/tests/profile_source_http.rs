//! Integration tests: profile-source `http` provider (Slice 1 of #11).
//!
//! Covers contracts C-PS4, C-PS5, C-PS6, C-PS7, C-PS8, C-PS14 from
//! `spec/profile-source.md`. The mock HTTP server pattern mirrors
//! `credential_http.rs` — small raw `TcpListener` server, controlled by the
//! test, returns canned responses.

mod mcp_common;

use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

/// A minimal YAML profile config the mock server can return.
///
/// Defines one bundle `baseline` (sh/bash/echo/cat/ls) and one profile `test`
/// using that bundle with `workspace` set to the given path. Matches the shape
/// of `mcp_common::write_mcp_config`'s output.
fn minimal_source_yaml(workspace: &str) -> String {
    format!(
        r#"bundles:
  baseline:
    binaries: [sh, bash, echo, cat, ls]

profiles:
  test:
    bundles: [baseline]
    filesystem:
      workspace: {workspace}
"#
    )
}

/// Same content as `minimal_source_yaml` but encoded as JSON. Mock server
/// returns this when testing the `application/json` content-type branch.
fn minimal_source_json(workspace: &str) -> String {
    serde_json::json!({
        "bundles": {
            "baseline": {
                "binaries": ["sh", "bash", "echo", "cat", "ls"]
            }
        },
        "profiles": {
            "test": {
                "bundles": ["baseline"],
                "filesystem": { "workspace": workspace }
            }
        }
    })
    .to_string()
}

/// Start a mock HTTP server on `127.0.0.1:0` that serves the given body with
/// the given `Content-Type` and status code. Optionally requires a specific
/// `Authorization` header — returns 401 if absent or mismatched.
///
/// Returns `(port, join_handle)`. The thread serves exactly one request and
/// returns the raw request string so the test can verify what was sent.
fn start_mock_http_server(
    body: String,
    content_type: &'static str,
    status: u16,
    required_auth: Option<String>,
) -> (u16, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
    let port = listener.local_addr().unwrap().port();

    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept connection");
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).expect("read request");
        let request = String::from_utf8_lossy(&buf[..n]).to_string();

        let (response_status, response_body) = if let Some(expected) = required_auth.as_ref() {
            let lc = request.to_lowercase();
            let has_auth = lc
                .lines()
                .any(|l| l.starts_with("authorization:") && l.contains(&expected.to_lowercase()));
            if has_auth {
                (status, body.clone())
            } else {
                (401, "unauthorized".to_string())
            }
        } else {
            (status, body.clone())
        };

        let response = format!(
            "HTTP/1.1 {response_status} OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
            response_body.len()
        );
        stream.write_all(response.as_bytes()).expect("write response");

        request
    });

    (port, handle)
}

/// Find a free port on localhost without listening on it. Used by failure
/// tests that need a "guaranteed connection-refused" address.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind free-port probe");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

// ─── C-PS4: http provider, YAML body ───

/// C-PS4: HTTP provider GETs the URL, parses a YAML body (Content-Type:
/// application/yaml), and the loaded profile is reachable end-to-end.
#[test]
fn http_provider_loads_yaml_body() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let yaml = minimal_source_yaml(workspace.path().to_str().unwrap());

    let (port, _server) = start_mock_http_server(yaml, "application/yaml", 200, None);

    let bootstrap = mcp_common::write_bootstrap_with_profile_source(&format!(
        r#"  provider: http
  url: "http://127.0.0.1:{port}/profiles"
  auth:
    type: none"#
    ));

    let mut client = mcp_common::McpClient::spawn(bootstrap.path());
    client.handshake();

    // Act
    let tools = client.tools_list();
    let response = client.call_tool("test", json!({ "command": "echo http-yaml-ok" }));

    // Assert — profile loaded from the YAML body is reachable and executes
    let tools_array = tools["result"]["tools"]
        .as_array()
        .expect("tools/list returns tools array");
    assert!(
        tools_array.iter().any(|t| t["name"].as_str() == Some("test")),
        "profile `test` from HTTP body should appear in tools/list, got: {:?}",
        tools_array
    );

    let result = &response["result"];
    let is_error = result["isError"].as_bool().unwrap_or(false);
    assert!(!is_error, "tools/call should not be isError, got: {:?}", result);
    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("http-yaml-ok"),
        "tools/call output should contain `http-yaml-ok`, got: {:?}",
        text
    );
}

// ─── C-PS5: http provider, JSON body ───

/// C-PS5: HTTP provider with `Content-Type: application/json` parses a JSON
/// body to the same logical content as the YAML case.
#[test]
fn http_provider_loads_json_body() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let body = minimal_source_json(workspace.path().to_str().unwrap());

    let (port, _server) = start_mock_http_server(body, "application/json", 200, None);

    let bootstrap = mcp_common::write_bootstrap_with_profile_source(&format!(
        r#"  provider: http
  url: "http://127.0.0.1:{port}/profiles"
  auth:
    type: none"#
    ));

    let mut client = mcp_common::McpClient::spawn(bootstrap.path());
    client.handshake();

    // Act
    let tools = client.tools_list();
    let response = client.call_tool("test", json!({ "command": "echo http-json-ok" }));

    // Assert
    let tools_array = tools["result"]["tools"]
        .as_array()
        .expect("tools/list returns tools array");
    assert!(
        tools_array.iter().any(|t| t["name"].as_str() == Some("test")),
        "profile `test` from JSON body should appear in tools/list, got: {:?}",
        tools_array
    );

    let result = &response["result"];
    let is_error = result["isError"].as_bool().unwrap_or(false);
    assert!(!is_error, "tools/call should not be isError, got: {:?}", result);
    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("http-json-ok"),
        "tools/call output should contain `http-json-ok`, got: {:?}",
        text
    );
}

// ─── C-PS6: http provider, bearer token from env var ───

/// C-PS6: When `auth: { type: static_secret, bearer_env: VAR }` is configured,
/// Ostia must send `Authorization: Bearer <value of VAR>` on the request.
/// Mock server returns 401 if the header is missing or wrong.
#[test]
fn http_provider_bearer_token_from_env_var() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let yaml = minimal_source_yaml(workspace.path().to_str().unwrap());
    let token = "expected-token-xyz";

    let (port, server_handle) = start_mock_http_server(
        yaml,
        "application/yaml",
        200,
        Some(format!("Bearer {token}")),
    );

    let bootstrap = mcp_common::write_bootstrap_with_profile_source(&format!(
        r#"  provider: http
  url: "http://127.0.0.1:{port}/profiles"
  auth:
    type: static_secret
    bearer_env: OSTIA_TEST_CONFIG_API_TOKEN"#
    ));

    let mut client = mcp_common::McpClient::spawn_with_args_and_env(
        bootstrap.path(),
        &[],
        &[("OSTIA_TEST_CONFIG_API_TOKEN", token)],
    );
    client.handshake();

    // Act
    let tools = client.tools_list();

    // Assert — handshake + tools/list succeeded
    let tools_array = tools["result"]["tools"]
        .as_array()
        .expect("tools/list returns tools array");
    assert!(
        tools_array.iter().any(|t| t["name"].as_str() == Some("test")),
        "profile `test` should be loaded when bearer token is present, got: {:?}",
        tools_array
    );

    // Assert — the mock server saw the bearer header
    let request = server_handle.join().expect("mock server thread");
    let lower = request.to_lowercase();
    assert!(
        lower
            .lines()
            .any(|l| l.starts_with("authorization:") && l.contains(&format!("bearer {token}"))),
        "request to mock server should contain `Authorization: Bearer {token}`, got: {:?}",
        request
    );
}

// ─── C-PS7: http provider, 500 response = startup failure ───

/// C-PS7: A 500 response from the source URL must cause `ostia serve` to
/// exit non-zero before becoming reachable. The client never gets a handshake
/// response.
#[test]
fn http_provider_500_response_blocks_startup() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let body = "internal server error".to_string();
    let (port, _server) = start_mock_http_server(body, "text/plain", 500, None);

    let bootstrap = mcp_common::write_bootstrap_with_profile_source(&format!(
        r#"  provider: http
  url: "http://127.0.0.1:{port}/profiles"
  auth:
    type: none"#
    ));

    // Act
    let outcome = mcp_common::spawn_or_capture_startup_failure(bootstrap.path(), &[]);

    // Assert
    match outcome {
        mcp_common::StdioStartupOutcome::ExitedEarly { status, stderr } => {
            assert!(
                !status.success(),
                "expected non-zero exit on HTTP 500, got: {:?}",
                status
            );
            let lower = stderr.to_lowercase();
            assert!(
                lower.contains("profile source")
                    || lower.contains("http")
                    || lower.contains("500"),
                "stderr should mention `profile source` / `http` / `500`, got: {:?}",
                stderr
            );
            let _ = workspace;
        }
        mcp_common::StdioStartupOutcome::Ready(_) => {
            panic!("expected ostia serve to exit on HTTP 500 from profile source");
        }
    }
}

// ─── C-PS8: http provider, connection refused = startup failure ───

/// C-PS8: When the profile source URL points at a port with nothing listening,
/// `ostia serve` must exit non-zero with stderr mentioning the URL or the
/// failure mode.
#[test]
fn http_provider_connection_refused_blocks_startup() {
    // Arrange — pick a free port and don't listen on it
    let port = free_port();
    let bootstrap = mcp_common::write_bootstrap_with_profile_source(&format!(
        r#"  provider: http
  url: "http://127.0.0.1:{port}/profiles"
  auth:
    type: none"#
    ));

    // Act
    let outcome = mcp_common::spawn_or_capture_startup_failure(bootstrap.path(), &[]);

    // Assert
    match outcome {
        mcp_common::StdioStartupOutcome::ExitedEarly { status, stderr } => {
            assert!(
                !status.success(),
                "expected non-zero exit on connection-refused, got: {:?}",
                status
            );
            let lower = stderr.to_lowercase();
            assert!(
                lower.contains("profile source")
                    || lower.contains("connection refused")
                    || lower.contains(&format!("{port}")),
                "stderr should mention `profile source` / `connection refused` / port, got: {:?}",
                stderr
            );
        }
        mcp_common::StdioStartupOutcome::Ready(_) => {
            panic!(
                "expected ostia serve to exit when profile source is unreachable on port {port}"
            );
        }
    }
}

// ─── C-PS14: http source reaches the real sandbox ───

/// C-PS14: A profile loaded from HTTP must reach the real sandbox enforcement
/// layer. Prove this by running a command that writes a file to the workspace
/// — the file should actually appear on disk after the call.
#[test]
fn http_source_executes_command_through_real_sandbox() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let ws_path = workspace.path().to_str().unwrap().to_string();
    let yaml = minimal_source_yaml(&ws_path);

    let (port, _server) = start_mock_http_server(yaml, "application/yaml", 200, None);

    let bootstrap = mcp_common::write_bootstrap_with_profile_source(&format!(
        r#"  provider: http
  url: "http://127.0.0.1:{port}/profiles"
  auth:
    type: none"#
    ));

    let mut client = mcp_common::McpClient::spawn(bootstrap.path());
    client.handshake();

    // Act — write a file inside the sandbox workspace
    let response = client.call_tool(
        "test",
        json!({
            "command": format!("echo sandbox-proof > {}/marker.txt", ws_path)
        }),
    );

    // Assert — the file actually appears on the host filesystem
    let result = &response["result"];
    let is_error = result["isError"].as_bool().unwrap_or(false);
    assert!(!is_error, "tools/call should not be isError, got: {:?}", result);

    let marker_path = workspace.path().join("marker.txt");
    let contents = std::fs::read_to_string(&marker_path)
        .expect("marker file should exist after sandbox write");
    assert!(
        contents.contains("sandbox-proof"),
        "sandbox-written file should contain `sandbox-proof`, got: {:?}",
        contents
    );
}
