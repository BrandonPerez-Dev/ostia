# Test Plan: Per-Profile MCP Tools (V10)

> Date: 2026-04-06

## Mock Boundaries

All controlled — Ostia owns the server, config, and sandbox. No external dependencies. Tests spawn real `ostia serve` processes (existing pattern from mcp_stdio/mcp_http tests).

## Vertical Slices

### Slice 1 (V0): Dynamic tools/list from config profiles

**User action:** Agent connects to Ostia MCP server, calls tools/list
**Path:** Client → McpServer::handle_request → dynamic tools_schema (reads config profiles, bundles)
**Mock boundary:** All real

**Integration test contracts:**

**C31: tools/list returns per-profile tools with correct descriptions**
- Setup: Config with 2 profiles. `baseline` bundle (no description). `dev-tools` bundle with `description: "git, curl, jq"`. Profile `test` (description: "Test sandbox", bundles: [baseline, dev-tools], workspace set). Profile `filtered` (bundles: [baseline, dev-tools], deny: ["rm *", "fakecmd *"] where rm is in baseline, fakecmd is not).
- Action: tools/list over stdio
- Expected:
  - Exactly 2 tools
  - Tool names: `test` and `filtered`
  - No `run_command` or `list_commands`
  - `test` tool description contains: "Test sandbox", "git", "curl", "jq", workspace path
  - `test` tool inputSchema has `command` property, does NOT have `profile` property
  - `filtered` tool description contains "rm" (notable denial)
  - `filtered` tool description does NOT contain "fakecmd" (non-notable denial)
- Error case: Config with zero profiles → tools/list returns empty tools array

---

### Slice 2 (V1): Profile tool dispatch

**User action:** Agent calls a profile-named tool with a command
**Path:** Client → McpServer::dispatch_tool (matches profile name) → exec_run_command (resolves profile, sandbox exec)
**Mock boundary:** All real

**Integration test contracts:**

**C32: Profile tool executes command**
- Setup: Config with profile `permissive` (baseline binaries, workspace set)
- Action: tools/call with name: "permissive", arguments: { command: "echo hi" }
- Expected: Content contains "hi", isError absent/false

**C33: Different profiles enforce different deny rules**
- Setup: Config with `permissive` (allows cat) and `restrictive` (denies "cat *"). Pre-create test.txt in workspace.
- Action: tools/call "permissive" with cat command → succeeds. tools/call "restrictive" with same cat command → denied.
- Expected: permissive returns file content. restrictive returns isError: true with denial message.

**C34: Unknown tool name returns error**
- Setup: Any valid config
- Action: tools/call with name: "nonexistent", arguments: { command: "echo" }
- Expected: isError: true, mentions unknown tool or profile

**C35: Missing command argument returns error**
- Setup: Any valid config with profile `permissive`
- Action: tools/call with name: "permissive", arguments: {}
- Expected: isError: true, mentions missing command

---

### Slice 3 (V2): Endpoint routing (HTTP)

**User action:** Agent connects to a profile-specific or grouped endpoint
**Path:** HTTP request → axum route /mcp/{name} → endpoint config lookup → scoped tools/list and tools/call
**Mock boundary:** All real

**Integration test contracts:**

**C36: Configured endpoint returns profile subset**
- Setup: Config with 3 profiles (alpha, beta, gamma), endpoints: { group: [alpha, beta] }. HTTP server.
- Action: tools/list on /mcp/group
- Expected: Exactly 2 tools named alpha and beta. No gamma.

**C37: Single profile name as endpoint**
- Setup: Same config (no endpoint named "gamma", but profile exists)
- Action: tools/list on /mcp/gamma
- Expected: Exactly 1 tool named gamma

**C38: Default /mcp returns all profiles**
- Setup: Same config
- Action: tools/list on /mcp
- Expected: 3 tools (alpha, beta, gamma)

**C39: Invalid endpoint returns error**
- Setup: Same config
- Action: tools/list on /mcp/nonexistent
- Expected: Error response (JSON-RPC error)

**C40: Execution scoped to endpoint**
- Setup: Same config, HTTP server
- Action: tools/call on /mcp/group with name: "gamma" → error. tools/call on /mcp/group with name: "alpha", arguments: { command: "echo works" } → succeeds.
- Expected: gamma call returns error (not available on this endpoint). alpha call returns "works".

## Test Infrastructure Notes

- New config writers needed in mcp_common: write_described_config, write_deny_filter_config, write_diff_rules_config, write_endpoint_config
- HTTP endpoint tests need helpers that hit /mcp/{name} paths (extend existing http_jsonrpc pattern)
- Existing tests (mcp_stdio, mcp_errors, mcp_http, mcp_auth, docker) will be rewritten in V3 to use the new interface
