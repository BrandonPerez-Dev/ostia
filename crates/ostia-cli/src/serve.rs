use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::State;
use axum::response::Json;
use ostia_core::binary::{
    fetch_bytes, BinaryCache, BinaryEntry, PostgresBlobParams, ResolvedBinaryRef,
};
use ostia_core::source::{CachedProfileSource, RefreshOutcome};
use ostia_core::{OstiaConfig, Profile};
use ostia_sandbox::SandboxExecutor;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::RwLock;

/// Default location of the on-disk binary cache when the operator hasn't set
/// `binary_cache_dir:` in bootstrap config. Matches the path documented in
/// `spec/binary-source.md`.
const DEFAULT_BINARY_CACHE_DIR: &str = "/var/lib/ostia/binaries";

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
    /// On-disk binary cache (`<binary_cache_dir>/<sha>/<name>`). `None` when
    /// the bootstrap config has no registered binaries AND no explicit
    /// `binary_cache_dir:` — Slice 2 backwards-compat path.
    binary_cache: Option<Arc<BinaryCache>>,
    user_id: Option<String>,
}

impl McpServer {
    fn new(
        config: OstiaConfig,
        cache: Option<Arc<CachedProfileSource>>,
        binary_cache: Option<Arc<BinaryCache>>,
        user_id: Option<&str>,
    ) -> Self {
        Self {
            config_state: Arc::new(RwLock::new(Arc::new(config))),
            cache,
            binary_cache,
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
                let new_binaries = sourced.binaries.clone();
                let new_config = OstiaConfig {
                    auth: prev.auth.clone(),
                    bundles: sourced.bundles,
                    profiles: sourced.profiles,
                    endpoints: prev.endpoints.clone(),
                    profile_source: prev.profile_source.clone(),
                    // Slice 3: source-provided binary registry replaces inline.
                    // Bootstrap's `binary_cache_dir` stays put.
                    binaries: sourced.binaries,
                    binary_cache_dir: prev.binary_cache_dir.clone(),
                };
                let pg_params = prev
                    .profile_source
                    .as_ref()
                    .and_then(|def| PostgresBlobParams::from_profile_source(def).ok());
                {
                    let mut guard = self.config_state.write().await;
                    *guard = Arc::new(new_config);
                }
                if !binary_diff.is_empty() {
                    eprintln!(
                        "info: binary diff: added={:?}, removed={:?}",
                        binary_diff.added, binary_diff.removed
                    );
                    // Slice 3: eager-pull each added binary in the background.
                    // Per-binary fail-open with a loud stderr warning.
                    if let Some(binary_cache) = self.binary_cache.as_ref() {
                        spawn_eager_pulls(
                            Arc::clone(binary_cache),
                            binary_diff.added.clone(),
                            new_binaries,
                            pg_params,
                        );
                    }
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
        // Slice 3: before constructing the sandbox, ensure every Cached
        // binary referenced by this profile is actually on disk in the
        // binary cache. On miss, fetch+stage synchronously (blocking the
        // call). Per-binary failures surface as a tool_error before any
        // sandbox setup so the caller sees a clean MCP-shaped error.
        let mut profile = match config
            .resolve_profile_with_identity(profile_name, self.user_id.as_deref())
        {
            Ok(p) => p,
            Err(e) => return tool_error(&format!("{}", e)),
        };

        if let Some(binary_cache) = self.binary_cache.as_ref() {
            if let Err(e) = self
                .ensure_cached_binaries(&mut profile, config, binary_cache.as_ref())
                .await
            {
                return tool_error(&format!("{}", e));
            }
            attach_cache_mounts(&mut profile, binary_cache.as_ref());
        }

        let command = command.to_string();
        let result = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
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

    /// Walk a profile's `resolved_binaries`. For each `Cached` entry that's
    /// missing from the on-disk cache, fetch from its source and stage. On
    /// failure for a referenced binary, return an error naming the binary so
    /// the caller can surface it as a tool_error.
    async fn ensure_cached_binaries(
        &self,
        profile: &mut Profile,
        config: &OstiaConfig,
        cache: &BinaryCache,
    ) -> anyhow::Result<()> {
        let pg_params = config
            .profile_source
            .as_ref()
            .and_then(|def| PostgresBlobParams::from_profile_source(def).ok());

        for resolved in &profile.resolved_binaries {
            if let ResolvedBinaryRef::Cached {
                name,
                sha256: _,
                entry,
            } = resolved
            {
                if !cache.is_cached(&entry.sha256, name, entry) {
                    fetch_and_stage(cache, name, entry, pg_params.as_ref())
                        .await
                        .map_err(|e| {
                            anyhow::anyhow!(
                                "binary `{}` is not available (pull failed): {}",
                                name,
                                e
                            )
                        })?;
                }
            }
        }
        Ok(())
    }
}

/// Free helper: walk `profile.resolved_binaries` and produce
/// (name, host_cache_path) entries for `Cached` variants the on-disk cache
/// can satisfy. Also collects tarball lib paths. Mutates the profile in place.
fn attach_cache_mounts(profile: &mut Profile, cache: &BinaryCache) {
    let mut mounts: Vec<(String, PathBuf)> = Vec::new();
    let mut lib_mounts: Vec<PathBuf> = Vec::new();
    for resolved in &profile.resolved_binaries {
        if let ResolvedBinaryRef::Cached {
            name,
            sha256: _,
            entry,
        } = resolved
        {
            let path = cache.entry_path(&entry.sha256, name, entry);
            if path.exists() {
                mounts.push((name.clone(), path));
                for lp in cache.lib_paths(&entry.sha256, entry) {
                    if lp.exists() {
                        lib_mounts.push(lp);
                    }
                }
            }
        }
    }
    profile.cache_mounts = mounts;
    profile.cache_lib_mounts = lib_mounts;
}

/// Fetch the binary's bytes from its source and stage them into the cache.
async fn fetch_and_stage(
    cache: &BinaryCache,
    name: &str,
    entry: &BinaryEntry,
    pg_params: Option<&PostgresBlobParams>,
) -> anyhow::Result<PathBuf> {
    let bytes = fetch_bytes(name, &entry.source, pg_params).await?;
    let cache = cache.clone();
    let name = name.to_string();
    let entry = entry.clone();
    let path = tokio::task::spawn_blocking(move || cache.stage(&name, &entry, &bytes))
        .await
        .map_err(|e| anyhow::anyhow!("internal join error: {}", e))??;
    Ok(path)
}

/// Spawn background eager-pull tasks for the binaries in `added`. Each task
/// fails open: a single pull failure logs a loud stderr warning but does not
/// block other binaries. Used by `refresh_if_due` after a successful refresh
/// observes a non-empty `BinaryDiff`.
fn spawn_eager_pulls(
    cache: Arc<BinaryCache>,
    added: Vec<String>,
    binaries: std::collections::HashMap<String, BinaryEntry>,
    pg_params: Option<PostgresBlobParams>,
) {
    for name in added {
        let Some(entry) = binaries.get(&name).cloned() else {
            continue;
        };
        let cache_clone = Arc::clone(&cache);
        let pg_params_clone = pg_params.clone();
        tokio::spawn(async move {
            // If already cached, skip. Avoids repulling on transient diffs.
            if cache_clone.is_cached(&entry.sha256, &name, &entry) {
                return;
            }
            match fetch_and_stage(cache_clone.as_ref(), &name, &entry, pg_params_clone.as_ref())
                .await
            {
                Ok(_) => {}
                Err(e) => {
                    eprintln!(
                        "warning: binary source pull failed for `{}`: {}",
                        name, e
                    );
                }
            }
        });
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

    // Slice 3: validate every profile resolves cleanly at startup. The
    // within-profile sha conflict check (C-BS5) bails here so the operator
    // sees a clean error before any listener binds. Errors carry the
    // binary name + profile name; the spec asserts those substrings.
    for profile_name in config.profiles.keys() {
        config
            .resolve_profile_with_identity(profile_name, None)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
    }

    // Slice 3: instantiate the binary cache when needed. The bootstrap config
    // signals intent via either an explicit `binary_cache_dir:` OR a non-empty
    // top-level `binaries:` registry. Pure Slice-2 configs (no registry, no
    // cache dir) skip the cache entirely so the legacy host-PATH path remains
    // unchanged.
    let binary_cache = if config.binary_cache_dir.is_some() || !config.binaries.is_empty() {
        let dir = config
            .binary_cache_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_BINARY_CACHE_DIR));
        match BinaryCache::new(&dir) {
            Ok(bc) => Some(Arc::new(bc)),
            Err(e) => {
                return Err(anyhow::anyhow!("binary cache: {}", e));
            }
        }
    } else {
        None
    };

    let server = Arc::new(McpServer::new(config, cache, binary_cache, user_id));

    match transport {
        "stdio" => serve_stdio(server).await,
        "http" => serve_http(server, host, port.unwrap_or(8080)).await,
        other => anyhow::bail!("unknown transport: {}", other),
    }
}
