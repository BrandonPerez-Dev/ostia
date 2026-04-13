# Capability: CLI

## Intent

`ostia` is a single Rust binary with three subcommands: `serve` (run the MCP server), `check` (validate config, show auth/binary status), and `run` (one-off sandboxed execution). The CLI is the primary dev-loop surface and the entry point for Docker images. It must produce clean output — `ostia run` is expected to be drop-in replaceable for `sh -c` in a pipeline, so no framework noise, tracing lines, or warning headers can leak into stdout.

## Subcommands

### `ostia serve`
- `--config <path>` — required, path to YAML config
- `--transport stdio|http` — default `stdio`
- `--host <addr>` — HTTP bind address, default `127.0.0.1`
- `--port <n>` — HTTP port. Precedence: `--port` flag > `OSTIA_PORT` env > default `8080`
- `--user-id <name>` — pin user identity for `http` credential provider templates

### `ostia check`
- `--config <path>` — validate parse + bundle references + credential config; optionally check auth/binary availability

### `ostia run`
- `--config <path>` + `--profile <name>` — select profile
- `-- <command>` — execute a single command in the profile's sandbox, with stdio wired through

## Test contracts — output cleanliness

### C-CO1: `ostia run` produces clean stdout
- **Test:** `stdout_contains_only_command_output` in `crates/ostia-cli/tests/cli_output.rs`
- **Setup:** `ostia run` with `echo hello`.
- **Action:** Execute.
- **Expected:** stdout is exactly `hello\n`; stderr is empty; exit 0. No `ostia:`, `ostia-debug:`, or `warning:` lines in stdout.

### C-CO2: Sandbox stderr passthrough has no injected lines
- **Test:** `stderr_passthrough_has_no_injected_lines` in `crates/ostia-cli/tests/cli_output.rs`
- **Setup:** `ostia run` with `bash -c 'echo err >&2; exit 1'`.
- **Action:** Execute.
- **Expected:** stdout empty; stderr contains `err`; no framework prefixes; exit non-zero.

### C-CO3: Mixed stdout and stderr stay separated
- **Test:** `mixed_stdout_stderr_stay_separated` in `crates/ostia-cli/tests/cli_output.rs`
- **Setup:** `ostia run` with `echo out && bash -c 'echo err >&2'`.
- **Action:** Execute.
- **Expected:** stdout contains `out` only; stderr contains `err` only. No cross-contamination. No framework lines. Exit 0.

## Test contracts — HTTP bind flags

### C-CB21: `--host` flag changes bind address
- **Test:** `mcp_http_host_flag_changes_bind_address` in `crates/ostia-cli/tests/mcp_http_bind.rs`
- **Setup:** `ostia serve --host 0.0.0.0 --port <port>`.
- **Action:** Connect via `127.0.0.1:<port>`.
- **Expected:** Server accepts the connection and completes the initialize handshake.

### C-CB22: Default host is localhost
- **Test:** `mcp_http_default_host_is_localhost` in `crates/ostia-cli/tests/mcp_http_bind.rs`
- **Setup:** `ostia serve --port <port>` (no `--host`).
- **Action:** Connect via `127.0.0.1:<port>`.
- **Expected:** Server accepts the connection.

### C-CB23: `OSTIA_PORT` env var sets the port
- **Test:** `mcp_http_env_var_sets_port` in `crates/ostia-cli/tests/mcp_http_bind.rs`
- **Setup:** `OSTIA_PORT=<port>` set in the parent env; `ostia serve` with no `--port` flag.
- **Action:** Wait for server to listen on `<port>`.
- **Expected:** Server binds to the env var port; initialize succeeds.

### C-CB24: `--port` flag overrides `OSTIA_PORT`
- **Test:** `mcp_http_port_flag_overrides_env_var` in `crates/ostia-cli/tests/mcp_http_bind.rs`
- **Setup:** Both `--port <flag_port>` and `OSTIA_PORT=<env_port>` set, with different values.
- **Action:** Connect to `flag_port` succeeds; connect to `env_port` fails.
- **Expected:** Server listens on `flag_port`; the env var is ignored when the flag is present.

## Invariants (untested but load-bearing)

- **`ostia run` is drop-in shell-replaceable.** Scripts piping output through `ostia run ... | jq ...` must work without filtering. Any tracing, progress, or debug output must go to stderr at most, never stdout.
- **Profile is locked per `ostia serve` process.** There is no CLI flag or runtime path to switch profiles mid-session.
- **`--config` is required on every subcommand.** There is no implicit `~/.ostia.yaml` lookup.

## Non-goals

- Interactive REPL. CLI is scripted-only.
- Profile authoring UI. Profiles are written as YAML by hand.
- `ostia init` scaffolding. Users copy from the examples directory.
