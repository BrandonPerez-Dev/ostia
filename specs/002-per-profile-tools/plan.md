# Plan: Per-Profile MCP Tools (V10)

> Date: 2026-04-06
> Status: planning

## What & Why

Replace the generic `run_command(profile, command)` / `list_commands(profile)` MCP interface with dynamic per-profile tools. Each config profile becomes its own MCP tool (`dev`, `readonly`, etc.) with a curated description that tells the LLM what CLIs are available, what's denied, and where the workspace is. Configurable endpoints let operators control which profiles each agent sees, keeping context tight.

## Constraints

- **Tool name = profile name.** `dev`, `readonly`. Claude Code namespaces as `mcp__ostia__dev`.
- **Tool schema: `command` only.** No `profile` arg. `inputSchema: { command: string }`.
- **Description auto-generated from config.** Profile `description` (opening line) + bundle `description` fields (featured tools) + notable denials + workspace path.
- **Notable denial = deny pattern whose binary is in the profile's resolved binaries.** If the agent wouldn't know about it, don't mention it.
- **Bundle `description` is optional.** Without it, bundle is silent (baseline). With it, text is featured in tool description.
- **Profile `description` is optional.** Falls back to profile name if absent.
- **`endpoints` is a new top-level config map.** Maps endpoint name → list of profile names.
- **Endpoint routing:** `/mcp` = all profiles. `/mcp/{name}` = endpoint config lookup → profile name fallback → error.
- **Stdio serves all profiles.** No endpoint concept on stdio.
- **`run_command` and `list_commands` removed.** Not deprecated — gone.
- **Token auth (`resolve_profile_from_token`) removed.** Profile determined by tool name. Endpoint URL = authorization.
- **Config schema additions:** `description: Option<String>` on `Bundle` and `ProfileDef`. `endpoints: HashMap<String, Vec<String>>` on `OstiaConfig`.

## Non-Goals

- HTTP-level auth (Bearer tokens on endpoints) — separate concern
- Credential provider / vault integration — separate feature (V11)
- Fixing symlinked binaries (python3) or script tools (npm) — separate bugfix
- Auto-generating Claude Code `.mcp.json` config — manual for now

## Build Skills (default for all verticals)

- rust-quality

## Verticals

### V0: Dynamic tools/list from config profiles
- **Does:** Replace static `tools_schema()` with dynamic generation — one tool per profile, description built from config fields
- **Done when:** `tools/list` returns N tools matching N profiles. Each tool has `name` = profile name, `inputSchema` with only `command`, and description containing featured bundle text + workspace path + notable denials. No `run_command` or `list_commands` tools.
- **Test:** Spawn server with multi-profile config. Assert tool count, names, description content, schema shape, absence of legacy tools. Assert notable denial filtering (deny of binary in profile → shown; deny of binary not in profile → omitted).
- **Deps:** None

### V1: Profile tool dispatch
- **Does:** `dispatch_tool` matches tool name against config profile names, extracts `command`, executes in that profile's sandbox
- **Done when:** `tools/call` with `name: "test"` and `{ command: "echo hi" }` executes and returns output. Nonexistent tool name returns error. Two profiles with different deny rules produce different results for same command.
- **Test:** Profile tool execution, unknown tool error, differential deny enforcement across profiles.
- **Deps:** V0

### V2: Endpoint routing (HTTP)
- **Does:** Add `/mcp/{name}` route. Looks up `endpoints` config first (multi-profile grouping), falls back to single profile name, errors if neither. Scopes `tools/list` and `tools/call` to that endpoint's profiles.
- **Done when:** `/mcp/group` with `endpoints: { group: [alpha, beta] }` returns 2 tools. `/mcp/alpha` (no endpoint config, profile exists) returns 1 tool. `/mcp/nonexistent` returns error. `/mcp` still returns all.
- **Test:** HTTP tests against each endpoint path, asserting tool count, names, and execution.
- **Deps:** V0, V1

### V3: Rewrite existing tests
- **Does:** Update all existing test files (mcp_stdio, mcp_errors, mcp_http, docker) to use profile-as-tool interface. Remove auth token tests (mcp_auth.rs).
- **Deps:** V0, V1

### V4: Rebuild Docker image with new config format (headline)
- **Deps:** V0, V1, V3

## Open Questions

- Should stdio support a `--profiles` flag to restrict to a subset? (Probably useful, defer to later.)
- How does credential provider (V11) interact with tool descriptions? (Auth status in annotations — designed but not built yet.)
