//! Integration tests: Docker image packaging (V9, Slice 2).
//!
//! Validates that the Docker image builds, runs as an HTTP MCP server,
//! executes sandboxed commands, and supports workspace volume mounts.
//!
//! These tests require Docker to be installed and running. They are
//! ignored by default — run with `cargo test -- --ignored` or
//! `cargo test docker --ignored`.

mod mcp_common;

use serde_json::{json, Value};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const DOCKER_IMAGE: &str = "ostia:test";

fn available_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// Check if Docker is available on this system.
fn docker_available() -> bool {
    Command::new("docker")
        .args(["info"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Build the Docker image from the project root. Panics on failure.
fn build_docker_image() {
    let project_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();

    let status = Command::new("docker")
        .args(["build", "-t", DOCKER_IMAGE, "."])
        .current_dir(project_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .status()
        .expect("failed to run docker build");

    assert!(
        status.success(),
        "docker build failed with exit code {:?}",
        status.code()
    );
}

/// A running Docker container handle. Kills and removes on drop.
struct DockerContainer {
    id: String,
}

impl Drop for DockerContainer {
    fn drop(&mut self) {
        Command::new("docker")
            .args(["kill", &self.id])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok();
    }
}

/// Start the Docker container in detached mode with HTTP transport.
/// Waits up to 10 seconds for the server to accept connections.
fn start_docker_http(port: u16, extra_args: &[&str]) -> DockerContainer {
    let port_mapping = format!("127.0.0.1:{}:8080", port);
    let mut args = vec![
        "run", "-d", "--rm",
        "--security-opt", "seccomp=unconfined",
        "--security-opt", "apparmor=unconfined",
        "-p", &port_mapping,
    ];
    args.extend_from_slice(extra_args);
    args.push(DOCKER_IMAGE);

    let output = Command::new("docker")
        .args(&args)
        .output()
        .expect("docker run -d");

    assert!(
        output.status.success(),
        "docker run -d failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let id = String::from_utf8(output.stdout)
        .expect("container ID is UTF-8")
        .trim()
        .to_string();

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        if TcpStream::connect(format!("127.0.0.1:{}", port)).is_ok() {
            return DockerContainer { id };
        }
        thread::sleep(Duration::from_millis(100));
    }
    // Cleanup on failure
    Command::new("docker").args(["kill", &id]).status().ok();
    panic!(
        "Docker container did not start accepting connections within 10 seconds on port {}",
        port
    );
}

fn http_jsonrpc(port: u16, request: &Value) -> Value {
    let body = serde_json::to_string(request).unwrap();
    let output = Command::new("curl")
        .args([
            "-s",
            "-X", "POST",
            &format!("http://127.0.0.1:{}/mcp", port),
            "-H", "Content-Type: application/json",
            "-d", &body,
        ])
        .output()
        .expect("run curl");

    let response = String::from_utf8(output.stdout).expect("curl output is UTF-8");
    serde_json::from_str(response.trim()).expect("parse JSON-RPC response from curl")
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
                "clientInfo": { "name": "docker-test", "version": "0.1.0" }
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

// ─── Contract 25: Image builds successfully ───

/// When docker build is run on the project root,
/// Then it exits 0 and produces a tagged image.
#[test]
#[ignore] // requires Docker
fn docker_image_builds_successfully() {
    if !docker_available() {
        eprintln!("skipping: docker not available");
        return;
    }

    // Act + Assert (build_docker_image panics on failure)
    build_docker_image();

    // Verify image exists
    let output = Command::new("docker")
        .args(["image", "inspect", DOCKER_IMAGE])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .expect("docker image inspect");

    assert!(
        output.status.success(),
        "image {} should exist after build",
        DOCKER_IMAGE
    );
}

// ─── Contract 26: MCP handshake over HTTP from host ───

/// When a client connects to the Docker container's HTTP port,
/// Then the MCP initialize handshake succeeds.
#[test]
#[ignore] // requires Docker
fn docker_mcp_handshake_over_http() {
    if !docker_available() {
        eprintln!("skipping: docker not available");
        return;
    }
    build_docker_image();

    // Arrange
    let port = available_port();
    let _container = start_docker_http(port, &[]);

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

    // Cleanup — DockerContainer kills on drop
    drop(_container);
}

// ─── Contract 27: Sandboxed command executes via HTTP ───

/// When a profile tool is called through the Docker container,
/// Then the command executes in the sandbox and output is returned.
#[test]
#[ignore] // requires Docker
fn docker_sandboxed_command_executes() {
    if !docker_available() {
        eprintln!("skipping: docker not available");
        return;
    }
    build_docker_image();

    // Arrange
    let port = available_port();
    let _container = start_docker_http(port, &[]);
    http_handshake(port);

    // Act
    let response = http_jsonrpc(
        port,
        &json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "tools/call",
            "params": {
                "name": "dev",
                "arguments": { "command": "echo docker-sandbox-works" }
            }
        }),
    );

    // Assert
    let result = &response["result"];
    assert!(
        result["isError"].is_null() || result["isError"] == false,
        "dev tool should succeed, got: {:?}",
        result
    );

    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("docker-sandbox-works"),
        "output should contain 'docker-sandbox-works', got: {:?}",
        text
    );

    // Cleanup — DockerContainer kills on drop
    drop(_container);
}

// ─── Contract 28: CLI tools available ───

/// When git, curl, and jq are invoked through the Docker container,
/// Then each tool executes and returns version output.
#[test]
#[ignore] // requires Docker
fn docker_cli_tools_available() {
    if !docker_available() {
        eprintln!("skipping: docker not available");
        return;
    }
    build_docker_image();

    // Arrange
    let port = available_port();
    let _container = start_docker_http(port, &[]);
    http_handshake(port);

    // Act — test three common CLI tools via the dev profile tool
    let tools = ["git --version", "curl --version", "jq --version"];
    for tool_cmd in &tools {
        let response = http_jsonrpc(
            port,
            &json!({
                "jsonrpc": "2.0",
                "id": 20,
                "method": "tools/call",
                "params": {
                    "name": "dev",
                    "arguments": { "command": tool_cmd }
                }
            }),
        );

        // Assert
        let result = &response["result"];
        assert!(
            result["isError"].is_null() || result["isError"] == false,
            "'{}' should succeed, got: {:?}",
            tool_cmd,
            result
        );

        let text = mcp_common::get_content_text(result);
        assert!(
            !text.is_empty(),
            "'{}' should produce output, got empty",
            tool_cmd
        );
    }

    // Cleanup — DockerContainer kills on drop
    drop(_container);
}

// ─── Contract 29: Denied command returns error ───

/// When a command uses a binary not in the profile,
/// Then isError is true.
#[test]
#[ignore] // requires Docker
fn docker_denied_command_returns_error() {
    if !docker_available() {
        eprintln!("skipping: docker not available");
        return;
    }
    build_docker_image();

    // Arrange
    let port = available_port();
    let _container = start_docker_http(port, &[]);
    http_handshake(port);

    // Act — python3 is not in the dev profile's bundles
    let response = http_jsonrpc(
        port,
        &json!({
            "jsonrpc": "2.0",
            "id": 30,
            "method": "tools/call",
            "params": {
                "name": "dev",
                "arguments": { "command": "python3 -c 'print(1)'" }
            }
        }),
    );

    // Assert
    let result = &response["result"];
    assert!(
        result["isError"] == true,
        "denied command should return isError: true, got: {:?}",
        result
    );

    // Cleanup — DockerContainer kills on drop
    drop(_container);
}

// ─── Contract 30: Workspace volume mount works ───

/// When a host directory is mounted as the workspace,
/// Then files from the host are readable inside the sandbox.
#[test]
#[ignore] // requires Docker
fn docker_workspace_volume_mount() {
    if !docker_available() {
        eprintln!("skipping: docker not available");
        return;
    }
    build_docker_image();

    // Arrange — create a temp dir with a file on the host
    let workspace = tempfile::tempdir().expect("create workspace");
    let input_file = workspace.path().join("input.txt");
    std::fs::write(&input_file, "host-file-content\n").expect("write input file");

    let ws_path = workspace.path().to_str().unwrap();
    let port = available_port();
    // :z relabels for SELinux; --user matches host UID for user namespace access
    let volume_arg = format!("{}:/workspace:ro,z", ws_path);
    let uid_arg = format!("{}:{}", nix::unistd::getuid(), nix::unistd::getgid());
    let _container = start_docker_http(port, &["-v", &volume_arg, "--user", &uid_arg]);
    http_handshake(port);

    // Act — read the host file from inside the sandbox
    let response = http_jsonrpc(
        port,
        &json!({
            "jsonrpc": "2.0",
            "id": 40,
            "method": "tools/call",
            "params": {
                "name": "dev",
                "arguments": { "command": "cat /workspace/input.txt" }
            }
        }),
    );

    // Assert
    let result = &response["result"];
    assert!(
        result["isError"].is_null() || result["isError"] == false,
        "cat should succeed, got: {:?}",
        result
    );

    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("host-file-content"),
        "output should contain 'host-file-content', got: {:?}",
        text
    );

    // Cleanup — DockerContainer kills on drop
    drop(_container);
}
