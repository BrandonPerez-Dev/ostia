/// Shared test infrastructure for MCP server integration tests.
///
/// Provides helpers for spawning the MCP server, managing the JSON-RPC
/// protocol, and writing test configs with various profile shapes.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub fn ostia_bin() -> PathBuf {
    env!("CARGO_BIN_EXE_ostia").into()
}

pub fn assert_user_namespaces() {
    let available = std::fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone")
        .map(|s| s.trim() == "1")
        .unwrap_or(true);

    assert!(
        available,
        "unprivileged user namespaces are required — check /proc/sys/kernel/unprivileged_userns_clone"
    );
}

/// MCP stdio client for integration tests.
///
/// Manages a child `ostia serve` process, sends JSON-RPC messages to its
/// stdin, and reads responses from its stdout. Kills the child on drop.
pub struct McpClient {
    pub child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpClient {
    /// Spawn `ostia serve --config <path>` and return a client handle.
    pub fn spawn(config_path: &Path) -> Self {
        let mut child = Command::new(ostia_bin())
            .args(["serve", "--config", config_path.to_str().unwrap()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn ostia serve");

        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));

        Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    /// Send a JSON-RPC message (request or notification).
    pub fn send(&mut self, msg: &Value) {
        let line = serde_json::to_string(msg).expect("serialize JSON-RPC message");
        writeln!(self.stdin, "{}", line).expect("write to stdin");
        self.stdin.flush().expect("flush stdin");
    }

    /// Read one JSON-RPC message from the server's stdout.
    pub fn recv(&mut self) -> Value {
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .expect("read from stdout");
        assert!(
            !line.is_empty(),
            "server closed stdout without responding"
        );
        serde_json::from_str(line.trim()).expect("parse JSON-RPC response")
    }

    /// Complete the MCP initialize handshake. Returns the initialize response.
    pub fn handshake(&mut self) -> Value {
        let id = self.next_id;
        self.next_id += 1;

        // Step 1: send initialize request
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "ostia-test", "version": "0.1.0" }
            }
        }));

        // Step 2: read initialize response
        let response = self.recv();

        // Step 3: send notifications/initialized (no response expected)
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }));

        response
    }

    /// Send a tools/list request and return the response.
    pub fn tools_list(&mut self) -> Value {
        let id = self.next_id;
        self.next_id += 1;

        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/list",
            "params": {}
        }));

        self.recv()
    }

    /// Send a tools/call request and return the response.
    pub fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;

        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments
            }
        }));

        self.recv()
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

/// Extract concatenated text from an MCP content array.
pub fn get_content_text(result: &Value) -> String {
    result["content"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Write a config with a single "test" profile and baseline binaries.
pub fn write_mcp_config(workspace: &str, extra_binaries: &[&str]) -> tempfile::NamedTempFile {
    let mut all_binaries = vec!["sh", "bash", "echo", "cat", "ls"];
    all_binaries.extend_from_slice(extra_binaries);
    let bins = all_binaries.join(", ");

    let config = format!(
        r#"bundles:
  baseline:
    binaries: [{bins}]

profiles:
  test:
    bundles: [baseline]
    filesystem:
      workspace: {workspace}
"#
    );
    let mut f = tempfile::NamedTempFile::new().expect("create temp config");
    std::io::Write::write_all(&mut f, config.as_bytes()).expect("write config");
    f
}

/// Write a config with a profile that has a failing auth check.
pub fn write_auth_fail_config(workspace: &str) -> tempfile::NamedTempFile {
    let config = format!(
        r#"bundles:
  baseline:
    binaries: [sh, bash, echo]

profiles:
  auth-test:
    bundles: [baseline]
    auth:
      fake-service:
        check: "false"
    filesystem:
      workspace: {workspace}
"#
    );
    let mut f = tempfile::NamedTempFile::new().expect("create temp config");
    std::io::Write::write_all(&mut f, config.as_bytes()).expect("write config");
    f
}

/// Write a config with two profiles for concurrent/multi-profile testing.
/// - alpha: allows sh, bash, echo
/// - beta: allows sh, bash, echo, cat
pub fn write_multi_profile_config(workspace: &str) -> tempfile::NamedTempFile {
    let config = format!(
        r#"bundles:
  alpha-tools:
    binaries: [sh, bash, echo]
  beta-tools:
    binaries: [sh, bash, echo, cat]

profiles:
  alpha:
    bundles: [alpha-tools]
    filesystem:
      workspace: {workspace}
  beta:
    bundles: [beta-tools]
    filesystem:
      workspace: {workspace}
"#
    );
    let mut f = tempfile::NamedTempFile::new().expect("create temp config");
    std::io::Write::write_all(&mut f, config.as_bytes()).expect("write config");
    f
}
