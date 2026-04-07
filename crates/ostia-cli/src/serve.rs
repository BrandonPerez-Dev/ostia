use std::path::Path;
use std::sync::Arc;

use axum::extract::State;
use axum::response::Json;
use ostia_core::OstiaConfig;
use ostia_sandbox::SandboxExecutor;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct McpServer {
    config: Arc<OstiaConfig>,
}

impl McpServer {
    fn new(config: OstiaConfig) -> Self {
        Self {
            config: Arc::new(config),
        }
    }

    async fn handle_request(&self, request: &Value) -> Option<Value> {
        let method = request["method"].as_str().unwrap_or("");

        // JSON-RPC 2.0: messages with an id are requests, without are notifications.
        let id = match request.get("id") {
            Some(id) if !id.is_null() => id.clone(),
            _ => return None,
        };

        match method {
            "initialize" => Some(jsonrpc_success(
                &id,
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "ostia", "version": env!("CARGO_PKG_VERSION") }
                }),
            )),
            "tools/list" => Some(jsonrpc_success(&id, json!({ "tools": self.profile_tools_schema(None) }))),
            "tools/call" => {
                let params = &request["params"];
                let name = params["name"].as_str().unwrap_or("");
                let arguments = &params["arguments"];
                let result = self.dispatch_tool(name, arguments).await;
                Some(jsonrpc_success(&id, result))
            }
            _ => Some(jsonrpc_error(&id, -32601, "Method not found")),
        }
    }

    async fn dispatch_tool(&self, name: &str, arguments: &Value) -> Value {
        match name {
            "run_command" => {
                let profile = match arguments["profile"].as_str() {
                    Some(p) => p,
                    None => return tool_error("missing required argument: profile"),
                };
                let command = match arguments["command"].as_str() {
                    Some(c) => c,
                    None => return tool_error("missing required argument: command"),
                };
                self.exec_run_command(profile, command).await
            }
            "list_commands" => {
                let profile = match arguments["profile"].as_str() {
                    Some(p) => p,
                    None => return tool_error("missing required argument: profile"),
                };
                self.exec_list_commands(profile)
            }
            _ => tool_error(&format!("unknown tool: {}", name)),
        }
    }

    async fn exec_run_command(&self, profile_name: &str, command: &str) -> Value {
        let config = self.config.clone();
        let profile_name = profile_name.to_string();
        let command = command.to_string();

        let result = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let profile = config.resolve_profile_from_token(&profile_name)?;
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

    fn exec_list_commands(&self, profile_name: &str) -> Value {
        match self.config.resolve_profile_from_token(profile_name) {
            Ok(profile) => {
                let mut binaries: Vec<&String> = profile.binaries.iter().collect();
                binaries.sort();
                let text = binaries
                    .iter()
                    .map(|b| b.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                tool_success(&text)
            }
            Err(e) => tool_error(&format!("{}", e)),
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

// ─── Tool schema ───

impl McpServer {
    /// Generate tools/list schema dynamically from config profiles.
    ///
    /// If `filter` is Some, only include profiles in the given set.
    /// If `filter` is None, include all profiles.
    fn profile_tools_schema(&self, filter: Option<&[&str]>) -> Value {
        let mut tools = Vec::new();
        let mut profile_names: Vec<&String> = self.config.profiles.keys().collect();
        profile_names.sort();

        for name in profile_names {
            if let Some(allowed) = filter {
                if !allowed.contains(&name.as_str()) {
                    continue;
                }
            }
            let profile_def = &self.config.profiles[name];
            let description = self.config.build_tool_description(name, profile_def);
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

        if let Some(response) = server.handle_request(&request).await {
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
        .handle_request(&request)
        .await
        .unwrap_or_else(|| json!({}));
    Json(response)
}

async fn serve_http(server: Arc<McpServer>, host: &str, port: u16) -> anyhow::Result<()> {
    let app = axum::Router::new()
        .route("/mcp", axum::routing::post(handle_http))
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
) -> anyhow::Result<()> {
    let config = OstiaConfig::load(config_path)?;
    let server = Arc::new(McpServer::new(config));

    match transport {
        "stdio" => serve_stdio(server).await,
        "http" => serve_http(server, host, port.unwrap_or(8080)).await,
        other => anyhow::bail!("unknown transport: {}", other),
    }
}
