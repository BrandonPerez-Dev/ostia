//! Integration tests: env and file credential providers (V3).
//!
//! Validates that the `env` provider reads a host env var and injects it
//! into the sandbox, and the `file` provider reads a host file and injects
//! its contents.

mod mcp_common;

use serde_json::json;
use std::io::Write;

/// V3 Slice 1: env provider injects host env var into sandbox
/// When a profile has `provider: env, env: "HOST_SECRET"`,
/// Then `echo $INJECTED_SECRET` inside the sandbox returns the host value.
#[test]
fn env_provider_injects_host_env_var() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_credential_config(
        workspace.path().to_str().unwrap(),
        r#"    credentials:
      my-env-cred:
        provider: env
        env: "HOST_SECRET"
        inject:
          INJECTED_SECRET: value"#,
    );
    let mut client = mcp_common::McpClient::spawn_with_env(
        config.path(),
        &[("HOST_SECRET", "super-secret-value")],
    );
    client.handshake();

    // Act
    let response = client.call_tool("test", json!({"command": "echo $INJECTED_SECRET"}));

    // Assert
    let result = &response["result"];
    let text = mcp_common::get_content_text(result);
    assert_eq!(
        text.trim(),
        "super-secret-value",
        "env provider should inject host env var into sandbox, got: {:?}",
        text
    );
}

/// V3 Slice 1 (error case): env provider with missing var produces error
/// When a profile has `provider: env, env: "NONEXISTENT_VAR"`,
/// Then execution is blocked with an error.
#[test]
fn env_provider_missing_var_blocks_execution() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_credential_config(
        workspace.path().to_str().unwrap(),
        r#"    credentials:
      bad-env:
        provider: env
        env: "OSTIA_NONEXISTENT_TEST_VAR_12345"
        inject:
          TOKEN: value"#,
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
        "missing env var should set isError: true, got: {:?}",
        result
    );
    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("not set") || text.contains("OSTIA_NONEXISTENT") || text.contains("missing"),
        "error should mention missing env var, got: {:?}",
        text
    );
}

/// V3 Slice 2: file provider injects host file contents into sandbox
/// When a profile has `provider: file, path: "/tmp/ostia-test-token.txt"`,
/// Then `echo $FILE_TOKEN` inside the sandbox returns the file contents.
#[test]
fn file_provider_injects_host_file_contents() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");

    // Write a temp file on the host with the secret
    let mut token_file = tempfile::NamedTempFile::new().expect("create token file");
    write!(token_file, "file-secret-123").expect("write token");
    let token_path = token_file.path().to_str().unwrap().to_string();

    let config = mcp_common::write_credential_config(
        workspace.path().to_str().unwrap(),
        &format!(
            r#"    credentials:
      my-file-cred:
        provider: file
        path: "{token_path}"
        inject:
          FILE_TOKEN: value"#
        ),
    );
    let mut client = mcp_common::McpClient::spawn(config.path());
    client.handshake();

    // Act
    let response = client.call_tool("test", json!({"command": "echo $FILE_TOKEN"}));

    // Assert
    let result = &response["result"];
    let text = mcp_common::get_content_text(result);
    assert_eq!(
        text.trim(),
        "file-secret-123",
        "file provider should inject file contents into sandbox, got: {:?}",
        text
    );
}

/// V3 Slice 2 (error case): file provider with missing file produces error
/// When a profile has `provider: file, path: "/nonexistent/file.txt"`,
/// Then execution is blocked with an error.
#[test]
fn file_provider_missing_file_blocks_execution() {
    // Arrange
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = mcp_common::write_credential_config(
        workspace.path().to_str().unwrap(),
        r#"    credentials:
      bad-file:
        provider: file
        path: "/nonexistent/ostia-test-file-12345.txt"
        inject:
          TOKEN: value"#,
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
        "missing file should set isError: true, got: {:?}",
        result
    );
    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("not found") || text.contains("No such file") || text.contains("nonexistent"),
        "error should mention missing file, got: {:?}",
        text
    );
}
