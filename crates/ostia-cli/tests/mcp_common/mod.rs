/// Shared test infrastructure for MCP server integration tests.
///
/// Provides helpers for spawning the MCP server, managing the JSON-RPC
/// protocol, and writing test configs with various profile shapes.

use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit},
    Aes256Gcm,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
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

    /// Spawn `ostia serve` with extra CLI args and optional env vars.
    ///
    /// Used for testing flags like `--user-id` or transport options.
    pub fn spawn_with_args_and_env(
        config_path: &Path,
        extra_args: &[&str],
        extra_env: &[(&str, &str)],
    ) -> Self {
        let mut cmd = Command::new(ostia_bin());
        cmd.args(["serve", "--config", config_path.to_str().unwrap()])
            .args(extra_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        for &(key, value) in extra_env {
            cmd.env(key, value);
        }

        let mut child = cmd.spawn().expect("spawn ostia serve");

        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));

        Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    /// Spawn `ostia serve` with additional env vars set on the child process.
    ///
    /// Used to test that parent env vars do NOT leak into the sandbox.
    pub fn spawn_with_env(config_path: &Path, extra_env: &[(&str, &str)]) -> Self {
        let mut cmd = Command::new(ostia_bin());
        cmd.args(["serve", "--config", config_path.to_str().unwrap()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        for &(key, value) in extra_env {
            cmd.env(key, value);
        }

        let mut child = cmd.spawn().expect("spawn ostia serve");

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

// ─── Env injection config writers (V0) ───

/// Write a config with explicit env vars on the profile for testing
/// sandbox env injection.
pub fn write_env_injection_config(
    workspace: &str,
    env_vars: &[(&str, &str)],
) -> tempfile::NamedTempFile {
    let env_entries: Vec<String> = env_vars
        .iter()
        .map(|(k, v)| format!("      {k}: \"{v}\""))
        .collect();
    let env_block = if env_entries.is_empty() {
        String::new()
    } else {
        format!("    env:\n{}\n", env_entries.join("\n"))
    };

    let config = format!(
        r#"bundles:
  baseline:
    binaries: [sh, bash, echo, cat, ls]

profiles:
  test:
    bundles: [baseline]
{env_block}    filesystem:
      workspace: {workspace}
"#
    );
    let mut f = tempfile::NamedTempFile::new().expect("create temp config");
    std::io::Write::write_all(&mut f, config.as_bytes()).expect("write config");
    f
}

// ─── Credential provider config writers (V1) ───

/// Write a config with a credential provider block on the profile.
///
/// The `credentials_yaml` parameter is a raw YAML fragment (already indented
/// at profile level) that gets spliced into the profile definition.
pub fn write_credential_config(
    workspace: &str,
    credentials_yaml: &str,
) -> tempfile::NamedTempFile {
    let config = format!(
        r#"bundles:
  baseline:
    binaries: [sh, bash, echo, cat, ls]

profiles:
  test:
    bundles: [baseline]
{credentials_yaml}
    filesystem:
      workspace: {workspace}
"#
    );
    let mut f = tempfile::NamedTempFile::new().expect("create temp config");
    std::io::Write::write_all(&mut f, config.as_bytes()).expect("write config");
    f
}

// ─── Auth token helpers ───

/// Generate a random 32-byte AES-256 key for token mode testing.
pub fn generate_auth_key() -> Vec<u8> {
    use aes_gcm::aead::OsRng;
    Aes256Gcm::generate_key(OsRng).to_vec()
}

/// Encrypt a profile name into a token: base64(nonce_12 || ciphertext || tag_16).
pub fn encrypt_profile(key: &[u8], profile: &str) -> String {
    use aes_gcm::aead::OsRng;
    let cipher = Aes256Gcm::new_from_slice(key).expect("valid 32-byte AES key");
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, profile.as_bytes())
        .expect("encrypt profile name");

    let mut token_bytes = Vec::with_capacity(12 + ciphertext.len());
    token_bytes.extend_from_slice(&nonce);
    token_bytes.extend_from_slice(&ciphertext);
    BASE64.encode(&token_bytes)
}

// ─── Token mode config writers ───

/// Write a config with token mode auth (AES-GCM encrypted profile tokens).
pub fn write_token_mode_config(workspace: &str, key: &[u8]) -> tempfile::NamedTempFile {
    let key_b64 = BASE64.encode(key);
    let config = format!(
        r#"auth:
  mode: token
  key: "{key_b64}"

bundles:
  baseline:
    binaries: [sh, bash, echo, cat, ls]

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

/// Write a token mode config with alpha and beta profiles.
pub fn write_token_mode_multi_config(workspace: &str, key: &[u8]) -> tempfile::NamedTempFile {
    let key_b64 = BASE64.encode(key);
    let config = format!(
        r#"auth:
  mode: token
  key: "{key_b64}"

bundles:
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

/// Write a config with explicit open mode auth.
pub fn write_open_mode_config(workspace: &str) -> tempfile::NamedTempFile {
    let config = format!(
        r#"auth:
  mode: open

bundles:
  baseline:
    binaries: [sh, bash, echo, cat, ls]

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

// ─── Per-profile tool config writers (V10) ───

/// Write a config with described bundles and profiles for testing
/// dynamic per-profile MCP tools.
/// - baseline bundle: no description (silent)
/// - dev-tools bundle: has description (featured)
/// - "test" profile: described, uses both bundles
pub fn write_described_config(workspace: &str) -> tempfile::NamedTempFile {
    let config = format!(
        r#"bundles:
  baseline:
    binaries: [sh, bash, echo, cat, ls]
  dev-tools:
    description: "git, curl, jq"
    binaries: [git, curl, jq]

profiles:
  test:
    description: "Test development sandbox"
    bundles: [baseline, dev-tools]
    filesystem:
      workspace: {workspace}
"#
    );
    let mut f = tempfile::NamedTempFile::new().expect("create temp config");
    std::io::Write::write_all(&mut f, config.as_bytes()).expect("write config");
    f
}

/// Write a config for testing that deny descriptions only show notable
/// denials (binaries that are actually in the profile).
/// - denies "rm *" (rm IS in baseline → notable, should appear)
/// - denies "fakecmd *" (fakecmd is NOT in any bundle → non-notable, should be omitted)
pub fn write_deny_filter_config(workspace: &str) -> tempfile::NamedTempFile {
    let config = format!(
        r#"bundles:
  baseline:
    binaries: [sh, bash, echo, cat, ls, rm]
  dev-tools:
    description: "git"
    binaries: [git]

profiles:
  filtered:
    description: "Deny filter test"
    bundles: [baseline, dev-tools]
    deny:
      - "rm *"
      - "fakecmd *"
    filesystem:
      workspace: {workspace}
"#
    );
    let mut f = tempfile::NamedTempFile::new().expect("create temp config");
    std::io::Write::write_all(&mut f, config.as_bytes()).expect("write config");
    f
}

/// Write a config with two profiles that have different deny rules,
/// for testing that different profile tools enforce different restrictions.
/// - "permissive": allows cat
/// - "restrictive": denies cat
pub fn write_diff_rules_config(workspace: &str) -> tempfile::NamedTempFile {
    let config = format!(
        r#"bundles:
  baseline:
    binaries: [sh, bash, echo, cat, ls]

profiles:
  permissive:
    description: "Allows cat"
    bundles: [baseline]
    filesystem:
      workspace: {workspace}
  restrictive:
    description: "Denies cat"
    bundles: [baseline]
    deny:
      - "cat *"
    filesystem:
      workspace: {workspace}
"#
    );
    let mut f = tempfile::NamedTempFile::new().expect("create temp config");
    std::io::Write::write_all(&mut f, config.as_bytes()).expect("write config");
    f
}

/// Write a config with endpoint mappings for testing custom endpoints.
/// - endpoint "group" maps to [alpha, beta]
/// - profiles: alpha, beta, gamma (gamma not in any endpoint)
pub fn write_endpoint_config(workspace: &str) -> tempfile::NamedTempFile {
    let config = format!(
        r#"bundles:
  baseline:
    binaries: [sh, bash, echo, cat, ls]

endpoints:
  group:
    - alpha
    - beta

profiles:
  alpha:
    description: "Alpha profile"
    bundles: [baseline]
    filesystem:
      workspace: {workspace}
  beta:
    description: "Beta profile"
    bundles: [baseline]
    filesystem:
      workspace: {workspace}
  gamma:
    description: "Gamma profile"
    bundles: [baseline]
    filesystem:
      workspace: {workspace}
"#
    );
    let mut f = tempfile::NamedTempFile::new().expect("create temp config");
    std::io::Write::write_all(&mut f, config.as_bytes()).expect("write config");
    f
}
