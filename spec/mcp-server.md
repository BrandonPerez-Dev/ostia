# Capability: MCP Server

## Intent

Ostia exposes its sandboxed shell environments as MCP tools over JSON-RPC, so any MCP-compatible client (Claude Code, Codex, Cursor, etc.) can drive them without code changes. Each config profile becomes a dynamic MCP tool named after the profile; the client calls it with a single `command` argument and gets stdout/stderr/exit back. Two transports are supported: stdio (for local agent integration) and HTTP (for remote and multi-tenant deployments).

## Surface

### `initialize`
Response includes:
- `protocolVersion` (string) — currently `"2024-11-05"`
- `capabilities.tools` (object) — empty marker
- `serverInfo.name` (string) — contains `"ostia"`
- `serverInfo.version` (string) — from `CARGO_PKG_VERSION`
- `instructions` (string, required) — dynamic server instructions explaining ostia's profile-as-sandbox model and listing the profiles visible to this client. The prose is a static preamble that tells the agent to inspect profile tools before concluding a capability is missing, explains that the `command` argument accepts POSIX shell syntax, and notes that each call runs in a fresh subprocess. The profile list is dynamic and **scope-aware**: when the request arrives via `POST /mcp/{endpoint}`, only profiles visible through that endpoint appear.

### `tools/list`
Returns one tool per visible profile. Each tool:
- `name` = profile name
- `description` = auto-built from profile's `description`, featured bundle descriptions, notable denials, and workspace path (see `build_tool_description` in `ostia-core::config`)
- `inputSchema` requires only `command: string`; does NOT accept a `profile` field

### `tools/call`
- `name` must match a visible profile
- `arguments.command` is required
- Command is executed in the named profile's sandbox and the result is returned as MCP `content`
- Non-zero exit codes are NOT `isError` — they're visible in the content text. Only sandbox denials and internal failures are `isError`.

### Endpoint routing (HTTP only)
- `POST /mcp` — all profiles visible
- `POST /mcp/{name}` — if `{name}` is in `endpoints: { {name}: [...] }`, scope to that profile subset. Else if `{name}` matches a profile, scope to that single profile. Else JSON-RPC error.
- Stdio transport has no endpoint concept — it always serves all profiles.

### Bind / transport flags
- `--transport stdio|http`
- `--host <addr>` — HTTP bind address (default `127.0.0.1`)
- `--port <n>` — HTTP port, overrides `OSTIA_PORT` env var; default `8080`

## Test contracts — stdio protocol

### C-C1: Initialize handshake
- **Test:** `mcp_initialize_handshake` in `crates/ostia-cli/tests/mcp_stdio.rs`
- **Setup:** Spawn stdio server with a valid config.
- **Action:** Client sends `initialize`.
- **Expected:** Response contains `protocolVersion` (string), `capabilities.tools` (object), `serverInfo.name` containing `"ostia"`.

### C-C1b: `initialize` includes dynamic `instructions` with profile list
- **Test:** `mcp_initialize_instructions_include_profile_list` in `crates/ostia-cli/tests/mcp_initialize_instructions.rs`
- **Setup:** Spawn stdio server with a two-profile config (`alpha`, `beta`).
- **Action:** Client sends `initialize`.
- **Expected:** `result.instructions` is a non-empty string; contains the case-insensitive substring `"ostia"`; contains `"alpha"`; contains `"beta"`. Validates both the static preamble (`"ostia"`) and the dynamic profile listing.

### C-C1c: Single-profile config renders cleanly
- **Test:** `mcp_initialize_instructions_single_profile_renders_cleanly` in `crates/ostia-cli/tests/mcp_initialize_instructions.rs`
- **Setup:** Spawn stdio server with a single-profile config (`test`).
- **Action:** Client sends `initialize`.
- **Expected:** `result.instructions` is non-empty; contains `"test"`; does NOT contain `"Available profiles (0)"`. Smoke-tests the count rendering and the single-profile branch.

### C-C2: `tools/list` returns per-profile tools
- **Test:** `mcp_tools_list_returns_expected_tools` in `crates/ostia-cli/tests/mcp_stdio.rs`
- **Setup:** Config with a single `test` profile; handshake complete.
- **Action:** `tools/list`.
- **Expected:** Exactly 1 tool named `test`. Tool `inputSchema.required` contains `command` but NOT `profile`.

### C-C4: Profile tool executes a sandboxed command
- **Test:** `mcp_run_command_executes` in `crates/ostia-cli/tests/mcp_stdio.rs`
- **Setup:** Config with `test` profile; handshake complete.
- **Action:** `tools/call` with `name="test"`, `arguments.command="echo hello"`.
- **Expected:** `isError` absent/false; output text contains `hello`.

## Test contracts — protocol errors

### C-C5: Denied command is an MCP error
- **Test:** `mcp_denied_command_returns_error` in `crates/ostia-cli/tests/mcp_errors.rs`
- **Setup:** Config where the `test` profile does not whitelist `curl`.
- **Action:** `tools/call` with `command="curl http://evil.com"`.
- **Expected:** `isError=true`; message mentions "denied", "not allowed", or "not whitelisted".

### C-C6: Unknown tool name is an MCP error
- **Test:** `mcp_invalid_profile_returns_error` in `crates/ostia-cli/tests/mcp_errors.rs`
- **Setup:** Valid config with a `test` profile.
- **Action:** `tools/call` with `name="nonexistent"`.
- **Expected:** `isError=true`; message mentions "unknown" or "not found".

### C-C7: Missing required argument is an error
- **Test:** `mcp_missing_required_argument` in `crates/ostia-cli/tests/mcp_errors.rs`
- **Setup:** Valid config; handshake complete.
- **Action:** `tools/call` with empty `arguments: {}`.
- **Expected:** Either `isError=true` in the result or a JSON-RPC error object.

### C-C8: Non-zero exit is visible but not an MCP error
- **Test:** `mcp_nonzero_exit_is_not_mcp_error` in `crates/ostia-cli/tests/mcp_errors.rs`
- **Setup:** Valid config; handshake complete.
- **Action:** `tools/call` with `command="exit 42"`.
- **Expected:** `isError` absent/false; output text contains `42`. Exit codes are data the agent can read, not protocol errors.

## Test contracts — HTTP transport

### C-C10: HTTP server initialize handshake
- **Test:** `mcp_http_initialize_handshake` in `crates/ostia-cli/tests/mcp_http.rs`
- **Setup:** Spawn HTTP MCP server.
- **Action:** Client POSTs `initialize` to `/mcp`.
- **Expected:** `protocolVersion` present; `serverInfo.name` contains `"ostia"`.

### C-C11: HTTP profile tool executes
- **Test:** `mcp_http_run_command_executes` in `crates/ostia-cli/tests/mcp_http.rs`
- **Setup:** HTTP server with `test` profile; client initializes.
- **Action:** `tools/call` with `command="echo http-works"`.
- **Expected:** `isError` absent/false; output contains `http-works`.

### C-C12: Concurrent clients on different profiles
- **Test:** `mcp_http_concurrent_clients_different_profiles` in `crates/ostia-cli/tests/mcp_http.rs`
- **Setup:** HTTP server with `alpha` and `beta` profiles.
- **Action:** Two threads concurrently call `tools/call` against different profiles.
- **Expected:** Both clients receive their correct responses (no cross-talk). `alpha` client gets `alpha-ok`; `beta` client gets `beta-ok`.

## Test contracts — per-profile tool shape

### C-C31: Tool description reflects profile config
- **Test:** `mcp_profile_tools_list_returns_per_profile_tools` in `crates/ostia-cli/tests/mcp_profile_tools.rs`
- **Setup:** Config with two profiles. `test` has a description, bundles `[baseline, dev-tools]` where `dev-tools` has its own `description`. `filtered` has `deny: ["rm *", "fakecmd *"]` where `rm` is in its bundles but `fakecmd` is not.
- **Action:** `tools/list`.
- **Expected:**
  - Exactly 2 tools named `test` and `filtered`.
  - No legacy `run_command` or `list_commands` tools.
  - `test` description contains profile description + featured bundle text + workspace path.
  - `filtered` description mentions `rm` (notable denial) but NOT `fakecmd` (non-notable).

### C-C31b: Empty tools list when no profiles configured
- **Test:** `mcp_profile_tools_list_empty_when_no_profiles` in `crates/ostia-cli/tests/mcp_profile_tools.rs`
- **Setup:** Config with bundles but no profiles.
- **Action:** `tools/list`.
- **Expected:** Empty `tools` array; no legacy tools present.

## Test contracts — dispatch behavior

### C-C32: Profile tool executes command in its sandbox
- **Test:** `mcp_profile_tool_executes_command` in `crates/ostia-cli/tests/mcp_profile_dispatch.rs`
- **Setup:** Config with `permissive` profile.
- **Action:** `tools/call` with `name="permissive"`, `command="echo hi"`.
- **Expected:** `isError` absent/false; output contains `hi`.

### C-C33: Different profiles enforce different deny rules
- **Test:** `mcp_profile_tools_enforce_different_deny_rules` in `crates/ostia-cli/tests/mcp_profile_dispatch.rs`
- **Setup:** Config with `permissive` (allows cat) and `restrictive` (denies `cat *`). A `test.txt` exists in the shared workspace.
- **Action:** `tools/call "permissive"` with a cat command succeeds; `tools/call "restrictive"` with the same command is denied.
- **Expected:** `permissive` result contains the file content; `restrictive` result has `isError=true`.

### C-C34: Unknown tool name returns error
- **Test:** `mcp_profile_tool_unknown_name_returns_error` in `crates/ostia-cli/tests/mcp_profile_dispatch.rs`
- **Setup:** Config with valid profiles; handshake complete.
- **Action:** `tools/call` with `name="nonexistent"`.
- **Expected:** `isError=true`; message mentions "unknown" or "not found".

### C-C35: Missing `command` argument returns error
- **Test:** `mcp_profile_tool_missing_command_returns_error` in `crates/ostia-cli/tests/mcp_profile_dispatch.rs`
- **Setup:** Config with `permissive` profile.
- **Action:** `tools/call` with `name="permissive"`, `arguments={}`.
- **Expected:** `isError=true`; message mentions `command`.

## Test contracts — endpoint routing

### C-C36: Configured endpoint returns profile subset
- **Test:** `mcp_endpoint_returns_profile_subset` in `crates/ostia-cli/tests/mcp_endpoints.rs`
- **Setup:** HTTP server with 3 profiles (`alpha`, `beta`, `gamma`) and `endpoints: { group: [alpha, beta] }`.
- **Action:** `tools/list` on `/mcp/group`.
- **Expected:** Exactly 2 tools named `alpha` and `beta`. `gamma` not present.

### C-C37: Single-profile endpoint fallback
- **Test:** `mcp_endpoint_single_profile_fallback` in `crates/ostia-cli/tests/mcp_endpoints.rs`
- **Setup:** Same config. `gamma` exists as a profile but not as a named endpoint.
- **Action:** `tools/list` on `/mcp/gamma`.
- **Expected:** Exactly 1 tool named `gamma`.

### C-C38: Default `/mcp` returns all profiles
- **Test:** `mcp_endpoint_default_returns_all_profiles` in `crates/ostia-cli/tests/mcp_endpoints.rs`
- **Setup:** HTTP server with 3 profiles.
- **Action:** `tools/list` on `/mcp`.
- **Expected:** 3 tools (`alpha`, `beta`, `gamma`).

### C-C39: Invalid endpoint returns JSON-RPC error
- **Test:** `mcp_endpoint_invalid_returns_error` in `crates/ostia-cli/tests/mcp_endpoints.rs`
- **Setup:** HTTP server with defined endpoints.
- **Action:** `tools/list` on `/mcp/nonexistent`.
- **Expected:** JSON-RPC error response.

### C-C39b: `initialize` instructions are scoped on endpoint
- **Test:** `mcp_initialize_instructions_are_endpoint_scoped` in `crates/ostia-cli/tests/mcp_initialize_instructions.rs`
- **Setup:** HTTP server with three profiles (`alpha`, `beta`, `gamma`) and `endpoints: { group: [alpha, beta] }`.
- **Action:** Client POSTs `initialize` to `/mcp/group`.
- **Expected:** `result.instructions` contains `"alpha"` and `"beta"`; does NOT contain `"gamma"`. The endpoint boundary that filters `tools/list` (see C-C36) also filters the initialize profile list — so an agent bound to an endpoint doesn't see profiles it cannot reach.

### C-C40: Execution scoped to endpoint
- **Test:** `mcp_endpoint_execution_scoped` in `crates/ostia-cli/tests/mcp_endpoints.rs`
- **Setup:** HTTP server with `endpoints: { group: [alpha, beta] }`; `gamma` exists as a profile but not in the group.
- **Action:** `tools/call` on `/mcp/group` with `name="gamma"` → denied. Same endpoint with `name="alpha"`, `command="echo works"` → succeeds.
- **Expected:** `gamma` call has `isError=true`; `alpha` call output contains `works`.

## Invariants (untested but load-bearing)

- **MCP `initialize` is the only place the server advertises itself.** Until a client calls `tools/list`, the initialize response is the agent's only signal that ostia exists and what profiles it has. (Issue #6 is the move to a dynamic `instructions` field — see `changes/005-mcp-initialize-instructions/`.)
- **Profile = tool name = authorization.** No per-call profile parameter, no per-call auth. The endpoint URL (HTTP) or the CLI flag (stdio) determines which profiles are reachable.
- **JSON-RPC notifications do not produce responses.** `notifications/initialized` (client → server) is accepted and silently consumed.

## Non-goals

- HTTP-level Bearer token auth on endpoints — tracked as issue #4, not yet built.
- Auth status annotations in `tools/list` — tracked as issue #3.
- MCP resources or prompts — ostia only exposes tools.
