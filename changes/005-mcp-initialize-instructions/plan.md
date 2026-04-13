# Plan: MCP Initialize Instructions (Issue #6)

> Date: 2026-04-13
> Status: implemented (retroactive spec landing)
> Issue: https://github.com/BrandonPerez-Dev/ostia/issues/6

## What & Why

When an MCP client connects to ostia, the `initialize` handshake response currently returns only `protocolVersion`, `capabilities`, and `serverInfo`. The client's agent has no idea what ostia does, what profiles are available, or that ostia is worth inspecting further. In practice, agents skip ostia entirely because its value proposition isn't visible until they call `tools/list`.

MCP's `initialize` response supports an `instructions` field — a natural-language string explaining what the server is for. Ostia should use it. The field should be **dynamic**: the prose is constant, but the profile list reflects the current config so an agent immediately sees what's available.

This is a small, surgical change scoped to `crates/ostia-cli/src/serve.rs`. The implementation has already landed; this plan retroactively documents it and lands test contracts in `spec/mcp-server.md`.

## Modifies spec files

- `spec/mcp-server.md` — adds 3 new contracts to the "stdio protocol" and "endpoint routing" sections. The `initialize` surface description is updated to include the `instructions` field as a required response property.

## Constraints

- **Instructions text is static prose.** Explains ostia's profile-as-sandbox model, nudges the agent to inspect profile tools, and describes the shell-syntax + fresh-subprocess execution semantics. The exact phrasing is committed to `serve.rs` as the source of truth.
- **Profile list is dynamic.** Built from `self.config.profiles` with the profile's `description` (falling back to profile name if absent).
- **Scope-aware on HTTP endpoint.** When the request arrives via `POST /mcp/{endpoint}`, the profile list is filtered to that endpoint's scope, matching how `tools/list` is already filtered.
- **No specific CLI names in the prose.** Don't hardcode `gh`, `aws`, `qbo` — those depend on user config and would rot.
- **No dependency additions.** The implementation is one new method on `McpServer` and one modified match arm.
- **The `instructions` field is advisory.** Clients that ignore it get the same behavior as before; clients that honor it get the preamble.

## Non-goals

- Not changing `tools/list` output (that's a separate capability improvement, tracked in issue #3 for auth status annotations).
- Not adding per-profile binary listings to the instructions — the profile description in `tools/list` is where the detailed tool info lives.
- Not adding a separate `server/about` JSON-RPC method — `initialize.instructions` is the MCP-native surface for this.
- Not translating the instructions for non-English clients. Single-language for now.

## Build skills (default for all verticals)

- rust-quality

## Verticals

### V0: Static preamble + dynamic profile list

- **Does:** Add `build_server_instructions()` to `McpServer`. Returns a string containing the static preamble (explains ostia, shell syntax, fresh subprocess semantics) followed by a dynamic `Available profiles (N):` list built from visible profiles. Wire it into the `initialize` match arm in `handle_request`. Scope parameter threads through from `handle_request`'s existing `filter` so HTTP endpoint routing filters the profile list naturally.
- **Done when:** `initialize` over stdio returns a non-empty `instructions` string that mentions "ostia" and every configured profile. `initialize` via `POST /mcp/{endpoint}` returns only the profiles scoped to that endpoint.
- **Test:** Three integration contracts in `spec/mcp-server.md` (see test-plan.md).
- **Deps:** None.

### V1: Documentation refresh (headline — deferred)

README's MCP section should mention the instructions field so users of the Docker image know what their clients will see. Not a blocker for V0 — tracked for the next docs pass.

## Open questions

None. The shape was validated by:
- Eyeballing the prose with the user before implementing (session: "alright, we should've done design skill ...")
- Testing against `test-config.yaml` (single profile) and `docker/config.yaml` (four profiles)
- Confirming the `filter` parameter threads correctly through `handle_request` for both stdio (no filter) and HTTP endpoint-scoped (filter = endpoint's profile list)
