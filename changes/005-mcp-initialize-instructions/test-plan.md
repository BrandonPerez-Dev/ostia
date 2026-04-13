# Test Plan: MCP Initialize Instructions

> Date: 2026-04-13
> Capability: mcp-server (`spec/mcp-server.md`)

## Mock Boundaries

All real. Same as every other MCP test — spawn `ostia serve` as a child process, speak real JSON-RPC. No mocks. See `context/testing-strategy.md`.

## Vertical Slices

### Slice V0: Dynamic `instructions` on `initialize`

**User action:** Agent connects via stdio or HTTP, sends `initialize`.
**Path:** Client → `McpServer::handle_request("initialize")` → `build_server_instructions(filter.as_deref())` → response.
**Mock boundary:** All real.

**Integration test contracts** (numbered to slot into `spec/mcp-server.md` after the existing C-C1…C-C8 stdio section and C-C36…C-C40 endpoint section):

---

### C-C1b: `initialize` includes a dynamic `instructions` field with profile list

- **Setup:** Spawn stdio server with `write_multi_profile_config` (two profiles: `alpha`, `beta`).
- **Action:** Client sends `initialize`.
- **Expected:**
  - `result.instructions` is a non-empty string.
  - `instructions` contains the case-insensitive substring `"ostia"` (the preamble identifies the server).
  - `instructions` contains the substring `"alpha"` (dynamic profile listing).
  - `instructions` contains the substring `"beta"` (dynamic profile listing).
- **Why this contract matters:** Without it, the instructions could regress to static-only or lose the profile names, and the agent-discovery guarantee (the whole point of the issue) would silently break.

---

### C-C1c: `initialize` instructions are scoped on HTTP endpoint

- **Setup:** HTTP server with three profiles (`alpha`, `beta`, `gamma`) and `endpoints: { group: [alpha, beta] }` — reuses `write_endpoint_config`.
- **Action:** Client POSTs `initialize` to `/mcp/group`.
- **Expected:**
  - `result.instructions` contains `"alpha"`.
  - `result.instructions` contains `"beta"`.
  - `result.instructions` does NOT contain `"gamma"`.
- **Why this contract matters:** The endpoint boundary is the authorization boundary (see `context/security-model.md` — "The model cannot change the profile"). If the `initialize` instructions listed profiles the agent cannot actually reach through that endpoint, the agent would have a misleading picture of its capabilities and might burn tool calls attempting unavailable profiles. The scope-filter must apply uniformly to both `tools/list` (already tested by C-C36) and `initialize.instructions` (new).

---

### C-C1d: `initialize` instructions render cleanly with a single profile

- **Setup:** Stdio server with `write_mcp_config` (single `test` profile).
- **Action:** Client sends `initialize`.
- **Expected:**
  - `result.instructions` is a non-empty string.
  - `instructions` contains `"test"` (the single profile name).
  - `instructions` does NOT contain the substring `"Available profiles (0)"` (smoke-test that the count is correct and the "no profiles" branch isn't taken).
- **Why this contract matters:** The single-profile case is the most common in practice (dev-loop use, examples in the README) and the easiest place to regress. A test with N=1 catches off-by-one and count-rendering mistakes that N=2 would mask.

## Test infrastructure notes

- No new helpers needed. `write_multi_profile_config`, `write_endpoint_config`, and `write_mcp_config` already exist in `mcp_common/mod.rs`.
- The HTTP scoped test reuses the helpers from `crates/ostia-cli/tests/mcp_endpoints.rs` (`http_handshake_path`, `http_jsonrpc_path`) — the initialize path returns the instructions in the standard JSON-RPC response body.
- All three contracts live in a single new file, `crates/ostia-cli/tests/mcp_initialize_instructions.rs`, for locality. Splitting across existing test files would scatter the V0 evidence and make future maintenance harder.

## Exit criteria

- All 3 contracts have executable tests.
- All tests are red before implementation, green after. (In this case, implementation already exists — so we write the tests, verify they pass against the landed implementation, and lock them in.)
- `spec/mcp-server.md` is edited in place to include the 3 new contracts alongside the existing C-C1…C-C40 set, and the `initialize` surface description mentions the `instructions` field.
