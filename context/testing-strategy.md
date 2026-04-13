# Testing Strategy

Ostia's test suite is almost entirely integration tests that spawn a real `ostia` process, hit its real MCP endpoints, and exercise real Linux namespaces. This doc explains why that's the right call, what the mock boundaries look like, and what test infrastructure exists to support it.

## The single mock boundary

**Mock external dependencies, not internal ones.** The only mocks in the Ostia test suite are:

- **HTTP servers** for credential provider tests. `crates/ostia-cli/tests/credential_http.rs` spins up a tiny Rust HTTP server on `127.0.0.1:<random-port>` that returns canned JSON. The server is controlled by the test, so we trust its responses — no need to mock the `http` provider's request logic.
- **Files on disk.** `credential_env_file.rs` writes tempfiles with known contents and points the `file` provider at them.
- **Host env vars.** `credential_env_file.rs` sets env vars on the parent `ostia serve` process for the `env` provider to read.

Everything else is real:

- Real `ostia serve` child process (spawned via `std::process::Command`)
- Real MCP JSON-RPC over real stdio pipes or real TCP
- Real mount namespaces (`unshare(CLONE_NEWNS)`)
- Real Landlock rulesets
- Real seccomp BPF filters
- Real file I/O inside the sandbox
- Real `goblin` ELF parsing against real binaries on the test host

The reason: Ostia is a security product. The bugs we care about are the kernel-interaction bugs — mount choreography, pivot_root ordering, Landlock ABI mismatches, seccomp rule interactions. A unit test with a mocked namespace would tell us nothing about whether those actually work.

## Why spawn a real child process

Two reasons:

1. **Namespace syscalls are destructive.** `unshare(CLONE_NEWNS)` mutates the calling process's namespace. If we ran sandbox construction inside the test binary itself, the test binary would be inside a namespace after the first test — and the next test would inherit a broken state. Forking a child (via `Command::spawn`) keeps the test binary clean.
2. **JSON-RPC over stdio needs a real pipe pair.** MCP's stdio transport reads from `stdin` line-by-line. Stubbing that in-process would mean re-implementing the transport to handle in-memory streams, which is a separate test surface we don't want.

The cost is that every integration test is slower than a unit test (each spawn is ~50-100ms) and some tests can't run in parallel cleanly. In practice the suite runs in ~30 seconds, which is acceptable for the guarantees we get.

## Test file layout

```
crates/ostia-cli/tests/
  mcp_common/mod.rs          # Shared McpClient, config writers, helpers
  mcp_stdio.rs               # Stdio initialize/list/call happy paths
  mcp_http.rs                # HTTP initialize/list/call, concurrency
  mcp_http_bind.rs           # --host / --port / OSTIA_PORT flag behavior
  mcp_profile_tools.rs       # tools/list per-profile shape + descriptions
  mcp_profile_dispatch.rs    # tools/call dispatch + deny enforcement
  mcp_endpoints.rs           # /mcp/{name} routing and scoping
  mcp_errors.rs              # Protocol error cases
  landlock.rs                # Filesystem enforcement
  seccomp.rs                 # Syscall filtering
  streaming.rs               # CLI-level real-time output
  builtins.rs                # Built-in bundle resolution + graceful degradation
  cli_output.rs              # stdout cleanliness for `ostia run`
  credential_providers.rs    # command provider happy + error paths
  credential_env_file.rs     # env + file providers
  credential_http.rs         # http provider + identity templating
  env_injection.rs           # execve env model (baseline vars, no parent leak)
  docker.rs                  # End-to-end Docker image tests

crates/ostia-core/tests/
  credential_presets.rs      # Built-in credential preset shape

crates/ostia-sandbox/tests/
  streaming_api.rs           # Programmatic streaming API (channel, collect)
```

## Shared infrastructure — `mcp_common/mod.rs`

Every MCP test imports helpers from `mcp_common`:

- `McpClient::spawn(config)` — starts a real `ostia serve --config <path>`, gives you a handle to send/receive JSON-RPC.
- `McpClient::spawn_with_args_and_env(config, args, env)` — for tests that need CLI flags (`--user-id`, `--transport http`) or custom env vars on the child.
- `handshake()` — sends initialize + notifications/initialized, returns initialize response.
- `tools_list()`, `call_tool(name, args)` — send one request, return one response.
- `get_content_text(result)` — extract the concatenated text content from an MCP result.

Config writers:

- `write_mcp_config(ws, extras)` — minimal config with one `test` profile.
- `write_multi_profile_config(ws)` — two profiles (`alpha`, `beta`).
- `write_described_config(ws)` — profile with description, featured bundles.
- `write_deny_filter_config(ws)` — profile with deny rules.
- `write_endpoint_config(ws)` — `endpoints:` block for routing tests.
- `write_env_injection_config(ws, env)` — profile with custom env vars.
- `write_credential_config(ws, creds_yaml)` — profile with a credential block spliced in.
- `write_token_mode_config(ws, key)` — profile with AES-GCM token auth (for HTTP transport tests).
- `write_open_mode_config(ws)` — explicit `auth: { mode: open }`.

All config writers return `NamedTempFile` so the file auto-deletes when the test finishes.

## Namespace prerequisite guard

Every test that enters a mount namespace starts with:

```rust
mcp_common::assert_user_namespaces();
```

which checks `/proc/sys/kernel/unprivileged_userns_clone == 1` and panics with a clear remediation message if namespaces are unavailable. This is a **hard guard**, not a silent skip — if the environment can't run the sandbox, the test must fail loudly so CI doesn't pretend to be green.

This was a retroactive fix — an earlier version used `if !available() { return }` which silently passed on misconfigured CI runners. The lesson is recorded as `B1` in the V6 test-quality revision (now landed in the spec): no silent skipping.

## What gets tested at each level

### `ostia-core` tests
- Credential preset shapes (`credential_presets.rs`)
- Config parsing edge cases (via the broader MCP test suite, not in dedicated unit tests)

The core crate has minimal direct tests because its surface is consumed by `ostia-cli`'s tests through the MCP server path. A resolved `Profile` is exercised end-to-end every time a tool call lands.

### `ostia-sandbox` tests
- Streaming API semantics (`streaming_api.rs` — channel, chunk order, collect wrapper, long-running command)

The sandbox crate's core is exercised through `ostia-cli`'s integration tests (landlock, seccomp, builtins) because those require a real command to run and stdout to check. Unit tests on `SandboxExecutor` would mostly be testing "does this Rust code compile" — the interesting behavior is the interaction with the kernel.

### `ostia-cli` tests
- Every user-facing contract. MCP protocol, CLI flags, sandbox enforcement, credential fetching, Docker packaging.

This is where 90% of the test surface lives and where spec contracts map one-to-one to test files.

## Docker tests

`docker.rs` builds and runs a real Docker image as part of the test. These tests are slow (~30s each to build + run) and require Docker to be installed on the test host. They're gated on a Docker availability check and will skip-with-loud-error if Docker isn't present, following the same "no silent skipping" rule.

They exist because the primary distribution mode is a Docker image, and bugs in the image build (missing binaries, bad base image, workspace mount behavior) wouldn't be caught by any other test.

## Why not property tests

Ostia's inputs are mostly not fuzz-amenable. The YAML config format is small and structured; the MCP JSON-RPC protocol is spec-defined and narrow; the sandbox input is arbitrary shell commands but the interesting failure modes are "does this kernel syscall work," not "does this parser handle weird input." A property test generating random YAML would mostly find parser bugs that `serde_yaml` already tests.

The one place property testing might pay off is subcommand pattern matching (glob patterns against arbitrary command strings). Not done yet; would be a worthwhile addition when the command matcher gets reworked for issue #8.

## Running the suite

```bash
cargo test                                    # all tests, all crates
cargo test -p ostia-cli                       # just CLI integration tests
cargo test -p ostia-cli --test mcp_endpoints  # one test file
cargo test -p ostia-cli mcp_run_command       # one test by name prefix
```

The CLI binary must be built before the integration tests run (they use `CARGO_BIN_EXE_ostia`). `cargo test` handles this automatically.

Docker tests are gated separately:

```bash
cargo test -p ostia-cli --test docker -- --ignored   # only if docker.rs uses #[ignore]
```

(Current state: docker tests run by default and check docker availability inline.)

## What this strategy rules out

- **Mocking the kernel.** We don't. The mount namespace is either real or the test doesn't run.
- **Mocking `ostia` itself.** No `MockSandboxExecutor`, no dependency injection seams. The test calls `ostia serve` and observes what comes out.
- **Testing the JSON-RPC layer in isolation.** The JSON-RPC handler is exercised through real stdio/TCP with real JSON. No in-memory transport fakes.

The upside is confidence: when `cargo test` is green, every spec contract has been verified against the real enforcement machinery. The downside is slower test runs and more flakiness vectors (port conflicts, tempfile cleanup, namespace setup races). Both have been tractable in practice.
