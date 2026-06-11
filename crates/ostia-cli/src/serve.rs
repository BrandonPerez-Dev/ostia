use std::path::Path;
use std::sync::Arc;

use axum::extract::State;
use axum::response::Json;
use ostia_core::source::{CachedProfileSource, RefreshOutcome};
use ostia_core::OstiaConfig;
use ostia_sandbox::SandboxExecutor;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::RwLock;

struct McpServer {
    /// Live OstiaConfig snapshot. Held behind an RwLock so that refreshes
    /// (write) and request handlers (read) can serialize cleanly. Inner Arc
    /// makes per-request snapshots cheap — readers clone the Arc, drop the
    /// RwLock guard, and work against an immutable snapshot.
    config_state: Arc<RwLock<Arc<OstiaConfig>>>,
    /// Cached profile source. `None` when the bootstrap config has no
    /// `profile_source:` block (legacy inline-only mode); in that case there
    /// is nothing to refresh.
    cache: Option<Arc<CachedProfileSource>>,
    user_id: Option<String>,
}

impl McpServer {
    fn new(
        config: OstiaConfig,
        cache: Option<Arc<CachedProfileSource>>,
        user_id: Option<&str>,
    ) -> Self {
        Self {
            config_state: Arc::new(RwLock::new(Arc::new(config))),
            cache,
            user_id: user_id.map(|s| s.to_string()),
        }
    }

    /// Cheap snapshot of the current config — clones the inner Arc.
    async fn current_config(&self) -> Arc<OstiaConfig> {
        self.config_state.read().await.clone()
    }

    /// If the cache is due (or there's no cache at all), check for a refresh.
    /// On success, atomically swap the McpServer's config to the new data and
    /// emit a binary diff to stderr for Slice 3 to consume. On failure, keep
    /// the existing config and emit a fail-open warning to stderr.
    async fn refresh_if_due(&self) {
        let Some(cache) = self.cache.as_ref() else {
            return;
        };
        match cache.refresh_if_due().await {
            RefreshOutcome::NotDue => {}
            RefreshOutcome::Refreshed {
                sourced,
                binary_diff,
            } => {
                // Build a new OstiaConfig by cloning the bootstrap concerns
                // off the existing snapshot, then overriding bundles+profiles
                // with the freshly-sourced data.
                let prev = self.current_config().await;
                let new_config = OstiaConfig {
                    auth: prev.auth.clone(),
                    bundles: sourced.bundles,
                    profiles: sourced.profiles,
                    endpoints: prev.endpoints.clone(),
                    profile_source: prev.profile_source.clone(),
                };
                {
                    let mut guard = self.config_state.write().await;
                    *guard = Arc::new(new_config);
                }
                if !binary_diff.is_empty() {
                    eprintln!(
                        "info: profile source refresh: binary set changed (added={:?}, removed={:?})",
                        binary_diff.added, binary_diff.removed
                    );
                }
            }
            RefreshOutcome::Failed { error } => {
                eprintln!("warning: profile source refresh failed: {}", error);
            }
        }
    }

    async fn handle_request(&self, request: &Value, scope: Option<&[String]>) -> Option<Value> {
        let method = request["method"].as_str().unwrap_or("");

        // JSON-RPC 2.0: messages with an id are requests, without are notifications.
        let id = match request.get("id") {
            Some(id) if !id.is_null() => id.clone(),
            _ => return None,
        };

        // Refresh BEFORE snapshotting the config so this request sees the
        // freshest data. Fail-open: a refresh failure logs but doesn't block.
        self.refresh_if_due().await;
        let config = self.current_config().await;

        let filter: Option<Vec<&str>> = scope.map(|s| s.iter().map(|p| p.as_str()).collect());

        match method {
            "initialize" => Some(jsonrpc_success(
                &id,
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "ostia", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": build_server_instructions(&config, filter.as_deref()),
                }),
            )),
            "tools/list" => Some(jsonrpc_success(
                &id,
                json!({ "tools": profile_tools_schema(&config, filter.as_deref()) }),
            )),
            "tools/call" => {
                let params = &request["params"];
                let name = params["name"].as_str().unwrap_or("");
                let arguments = &params["arguments"];
                let result = self
                    .dispatch_tool(&config, name, arguments, filter.as_deref())
                    .await;
                Some(jsonrpc_success(&id, result))
            }
            _ => Some(jsonrpc_error(&id, -32601, "Method not found")),
        }
    }

    /// Resolve an endpoint name to a list of profile names.
    ///
    /// Checks config.endpoints first (multi-profile grouping), then falls
    /// back to a single profile name, then returns None.
    async fn resolve_endpoint(&self, name: &str) -> Option<Vec<String>> {
        let config = self.current_config().await;
        if let Some(profiles) = config.endpoints.get(name) {
            return Some(profiles.clone());
        }
        if config.profiles.contains_key(name) {
            return Some(vec![name.to_string()]);
        }
        None
    }

    async fn dispatch_tool(
        &self,
        config: &Arc<OstiaConfig>,
        name: &str,
        arguments: &Value,
        allowed_profiles: Option<&[&str]>,
    ) -> Value {
        if !config.profiles.contains_key(name) {
            return tool_error(&format!("unknown tool: {}", name));
        }

        if let Some(allowed) = allowed_profiles {
            if !allowed.contains(&name) {
                return tool_error(&format!("tool '{}' is not available on this endpoint", name));
            }
        }

        let command = match arguments["command"].as_str() {
            Some(c) => c,
            None => return tool_error("missing required argument: command"),
        };

        self.exec_in_profile(config, name, command).await
    }

    async fn exec_in_profile(
        &self,
        config: &Arc<OstiaConfig>,
        profile_name: &str,
        command: &str,
    ) -> Value {
        let config = config.clone();
        let profile_name = profile_name.to_string();
        let command = command.to_string();
        let user_id = self.user_id.clone();

        let result = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let profile = config.resolve_profile_with_identity(&profile_name, user_id.as_deref())?;
            let executor = SandboxExecutor::from_profile(profile)?;
            executor.execute(&command)
        })
        .await;

        match result {
            Ok(Ok(exec)) if !exec.allowed => {
                tool_error(&exec.reason.unwrap_or_else(|| "command denied".into()))
            }
            Ok(Ok(exec)) => {
                let mut text = exec.stdout;
                if !exec.stderr.is_empty() {
                    if !text.is_empty() && !text.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str(&format!("stderr: {}", exec.stderr));
                }
                if exec.exit_code != 0 {
                    if !text.is_empty() && !text.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str(&format!("exit code: {}", exec.exit_code));
                }
                tool_success(&text)
            }
            Ok(Err(e)) => tool_error(&format!("{}", e)),
            Err(e) => tool_error(&format!("internal error: {}", e)),
        }
    }
}

// ─── JSON-RPC helpers ───

fn jsonrpc_success(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn jsonrpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

// ─── MCP tool result helpers ───

fn tool_success(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }] })
}

fn tool_error(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": true })
}

// ─── Server instructions ───

/// Build the MCP `initialize.instructions` string.
///
/// Dynamic: lists the profiles visible to this client (scope-aware when
/// mounted at `/mcp/{endpoint}`). The preamble is static prose that
/// explains the profile-as-sandbox model and nudges the agent to inspect
/// profile tools before assuming a CLI is unavailable.
fn build_server_instructions(config: &OstiaConfig, scope: Option<&[&str]>) -> String {
    let preamble = "\
ostia exposes sandboxed shell environments as MCP tools. Each tool in tools/list \
is a *profile* — a curated set of allowed binaries, filesystem scope, network \
rules, and pre-injected credentials. Calling a profile tool with a `command` \
argument runs that command inside the profile's sandbox and returns \
stdout/stderr.

Before concluding you can't do something, inspect the available profile tools. \
Profiles commonly bundle CLIs that aren't otherwise available to you, \
pre-authenticated via the profile's credential configuration — so \
domain-specific tools (cloud SDKs, SaaS clients, internal utilities) may \
already be ready to use without extra setup. Read each profile's description \
to see what it's for.

The `command` argument accepts POSIX shell syntax: pipes, redirects, command \
substitution, and `&&`/`||` chains. Each call runs in a fresh subprocess, so \
shell state (cwd, exported variables) does not persist between calls — combine \
operations within a single command when they need to share state.";

    let mut names: Vec<&String> = config.profiles.keys().collect();
    names.sort();
    let visible: Vec<&String> = names
        .into_iter()
        .filter(|n| match scope {
            Some(allowed) => allowed.contains(&n.as_str()),
            None => true,
        })
        .collect();

    let mut out = String::with_capacity(preamble.len() + 256);
    out.push_str(preamble);
    out.push_str("\n\n");

    if visible.is_empty() {
        out.push_str("No profiles are available in this scope.");
    } else {
        out.push_str(&format!("Available profiles ({}):\n", visible.len()));
        for name in visible {
            let desc = config
                .profiles
                .get(name)
                .and_then(|p| p.description.as_deref())
                .unwrap_or(name);
            out.push_str(&format!("  - {}: {}\n", name, desc));
        }
    }

    out
}

// ─── Tool schema ───

/// Generate tools/list schema dynamically from config profiles.
///
/// If `filter` is Some, only include profiles in the given set.
/// If `filter` is None, include all profiles.
fn profile_tools_schema(config: &OstiaConfig, filter: Option<&[&str]>) -> Value {
    let mut tools = Vec::new();
    let mut profile_names: Vec<&String> = config.profiles.keys().collect();
    profile_names.sort();

    for name in profile_names {
        if let Some(allowed) = filter {
            if !allowed.contains(&name.as_str()) {
                continue;
            }
        }
        let profile_def = &config.profiles[name];
        let description = config.build_tool_description(name, profile_def);
        tools.push(json!({
            "name": name,
            "description": description,
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute"
                    }
                },
                "required": ["command"]
            }
        }));
    }

    json!(tools)
}

// ─── Stdio transport ───

async fn serve_stdio(server: Arc<McpServer>) -> anyhow::Result<()> {
    let stdin = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();
    let mut lines = stdin.lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let request: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if let Some(response) = server.handle_request(&request, None).await {
            let out = serde_json::to_string(&response).unwrap();
            stdout.write_all(out.as_bytes()).await.ok();
            stdout.write_all(b"\n").await.ok();
            stdout.flush().await.ok();
        }
    }

    Ok(())
}

// ─── HTTP transport ───

async fn handle_http(
    State(server): State<Arc<McpServer>>,
    body: String,
) -> Json<Value> {
    let request: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => {
            return Json(json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": { "code": -32700, "message": "Parse error" }
            }));
        }
    };

    let response = server
        .handle_request(&request, None)
        .await
        .unwrap_or_else(|| json!({}));
    Json(response)
}

async fn handle_http_endpoint(
    State(server): State<Arc<McpServer>>,
    axum::extract::Path(endpoint_name): axum::extract::Path<String>,
    body: String,
) -> Json<Value> {
    // Resolve endpoint to profile list. `resolve_endpoint` is async now since
    // it consults the live config cache.
    let scope = match server.resolve_endpoint(&endpoint_name).await {
        Some(profiles) => profiles,
        None => {
            // Extract request id for the error response
            let id = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v.get("id").cloned())
                .unwrap_or(Value::Null);
            return Json(jsonrpc_error(
                &id,
                -32001,
                &format!("unknown endpoint: {}", endpoint_name),
            ));
        }
    };

    let request: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => {
            return Json(json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": { "code": -32700, "message": "Parse error" }
            }));
        }
    };

    let response = server
        .handle_request(&request, Some(&scope))
        .await
        .unwrap_or_else(|| json!({}));
    Json(response)
}

async fn serve_http(server: Arc<McpServer>, host: &str, port: u16) -> anyhow::Result<()> {
    let app = axum::Router::new()
        .route("/mcp", axum::routing::post(handle_http))
        .route("/mcp/{name}", axum::routing::post(handle_http_endpoint))
        .with_state(server);

    let listener = tokio::net::TcpListener::bind(format!("{}:{}", host, port)).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

// ─── Public entry point ───

pub async fn run_serve(
    config_path: &Path,
    transport: &str,
    host: &str,
    port: Option<u16>,
    user_id: Option<&str>,
) -> anyhow::Result<()> {
    // Use the source-resolving loader so `profile_source:` blocks in the
    // bootstrap YAML actually dispatch and populate bundles + profiles.
    // Errors here (unreachable source, bad response, auth failure) propagate
    // up to `main()` which exits non-zero with the message prefixed `error:`.
    // The cache is returned alongside so per-request handlers can refresh
    // when TTL expires.
    let (config, cache) = OstiaConfig::load_resolved_with_cache(config_path)
        .await
        .map_err(|e| anyhow::anyhow!("profile source: {}", e))?;
    let server = Arc::new(McpServer::new(config, cache, user_id));

    match transport {
        "stdio" => serve_stdio(server).await,
        "http" => serve_http(server, host, port.unwrap_or(8080)).await,
        other => anyhow::bail!("unknown transport: {}", other),
    }
}
