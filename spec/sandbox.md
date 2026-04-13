# Capability: Sandbox

## Intent

The sandbox is the OS enforcement layer. Every command a profile runs is executed inside a mount namespace where only whitelisted binaries and their shared libraries exist; Landlock restricts filesystem access; seccomp blocks namespace-breakout syscalls; output streams to the caller in real time. Binaries not in the whitelist literally don't exist in the agent's filesystem view — this cannot be bypassed by shell tricks, encoding, symlinks, or subshells because the attack surface is at the kernel level.

See `context/sandbox-internals.md` for mount choreography and `context/security-model.md` for the two-layer trust model.

## Enforcement layers

1. **Mount namespace binary allowlisting (hard).** `unshare(CLONE_NEWNS)` + tmpfs root + bind-mount the whitelisted binaries and ELF deps resolved via goblin. After `pivot_root`, unlisted binaries are genuinely absent.
2. **Subcommand pattern matching (defense-in-depth).** Before exec, the command string is shell-split on `&&`/`||`/`;`/`|` and each subcommand is validated against the profile's glob patterns. Rejection happens before the sandbox is entered.
3. **Landlock filesystem rules.** Constructed from profile `filesystem.workspace` (rw), `filesystem.read` (ro), `filesystem.deny_read`, `filesystem.deny_write`. Applied after `pivot_root`. Deny paths override any allow.
4. **Seccomp BPF filter.** Blocks `mount`, `unshare`, `clone` with namespace flags, `ptrace`, `kexec_load`, `open_by_handle_at`. `prctl(PR_SET_NO_NEW_PRIVS, 1)` is set before exec.
5. **Network namespace (untested, partial).** `CLONE_NEWNET` + host-side HTTP/SOCKS proxy is designed but not yet covered by integration tests. No contracts below; see `context/sandbox-internals.md` for the planned shape.

## Execution model

- **`execve`, not `execvp`.** The sandbox constructs an explicit env vector: baseline `PATH=/usr/bin:/bin`, `HOME=/`, `TERM=dumb`, plus any profile `env:` entries and credential injections. Parent process env vars are never inherited.
- **Streaming output.** `execute_streaming` returns a channel of `Chunk::Stdout(bytes)` / `Chunk::Stderr(bytes)` / `Chunk::Exit(code)` events. `execute_streaming_collect` wraps the channel into an `ExecutionResult` for callers that want a buffered result. CLI (`ostia run`) pipes chunks directly through stdio.

## Test contracts — filesystem isolation (Landlock)

### C-S1: Workspace writes succeed
- **Test:** `write_to_workspace_succeeds` in `crates/ostia-cli/tests/landlock.rs`
- **Setup:** Profile with `workspace: <ws_path>` and baseline binaries.
- **Action:** `ostia run` with `echo hello > <ws_path>/testfile && cat <ws_path>/testfile`.
- **Expected:** stdout `hello`; stderr empty; exit 0.

### C-S2: Writes outside workspace fail
- **Test:** `write_outside_workspace_fails_with_permission_error` in `crates/ostia-cli/tests/landlock.rs`
- **Setup:** Profile with workspace set; no other write paths.
- **Action:** `ostia run` with `echo pwned > /tmp/ostia-landlock-outside`.
- **Expected:** stdout empty; stderr contains "Permission denied" or "Read-only file system"; exit non-zero. Failure reason is checked in stderr, not just exit code.

### C-S3a: Read path reads succeed
- **Test:** `read_path_read_succeeds` in `crates/ostia-cli/tests/landlock.rs`
- **Setup:** Profile with `workspace` + `read: [<read_dir>]`; `<read_dir>/data.txt` contains `read-only-content`.
- **Action:** `ostia run` with `cat <read_dir>/data.txt`.
- **Expected:** stdout `read-only-content`; exit 0.

### C-S3b: Read path writes fail
- **Test:** `read_path_write_fails` in `crates/ostia-cli/tests/landlock.rs`
- **Setup:** Same as S3a.
- **Action:** `ostia run` with `echo pwned > <read_dir>/hacked.txt`.
- **Expected:** stdout empty; stderr contains "Permission denied" or "Read-only file system"; exit non-zero.

### C-S4: Mandatory deny paths block reads
- **Test:** `mandatory_deny_path_is_blocked` in `crates/ostia-cli/tests/landlock.rs`
- **Setup:** Profile with `read: [.ssh]` — deny rules must override the read path. Fixture `.ssh/id_rsa` contains a secret string.
- **Action:** `ostia run` with `cat .ssh/id_rsa`.
- **Expected:** stdout empty; exit non-zero. The deny list wins over the read allow.

### C-S5: Sensitive paths not visible
- **Test:** `sensitive_paths_not_visible_in_sandbox` in `crates/ostia-cli/tests/landlock.rs`
- **Setup:** Default sandbox — `/etc/shadow` is neither in `workspace` nor `read`.
- **Action:** `ostia run` with `cat /etc/shadow`.
- **Expected:** stdout empty; stderr contains "No such file" (NOT "Permission denied") — mount namespace genuinely does not expose the file, so the error is absence, not policy. exit non-zero.

## Test contracts — syscall filtering (seccomp)

### C-SC1a: Normal commands run with seccomp applied
- **Test:** `normal_commands_work_with_seccomp` in `crates/ostia-cli/tests/seccomp.rs`
- **Setup:** Profile with baseline binaries + `date`.
- **Action:** `ostia run` with `echo hello && echo world && date +%Y`.
- **Expected:** stdout contains `hello`, `world`, a 4-digit year; exit 0. Seccomp does not block ordinary commands.

### C-SC1b: Workspace file I/O works with seccomp
- **Test:** `workspace_io_works_with_seccomp` in `crates/ostia-cli/tests/seccomp.rs`
- **Setup:** Profile with workspace set.
- **Action:** `ostia run` with `echo seccomp-test > <ws_path>/file.txt && cat <ws_path>/file.txt`.
- **Expected:** stdout `seccomp-test`; exit 0.

### C-SC2: Namespace creation is blocked
- **Test:** `seccomp_blocks_unshare` in `crates/ostia-cli/tests/seccomp.rs`
- **Setup:** `unshare` binary whitelisted in the profile.
- **Action:** `ostia run` with `unshare --mount echo escaped`.
- **Expected:** stdout empty (the nested `echo` never runs); stderr contains "Operation not permitted" or "cannot"; exit non-zero. Even with `unshare` binary available, the syscall is denied.

### C-SC3: Mount is blocked
- **Test:** `seccomp_blocks_mount` in `crates/ostia-cli/tests/seccomp.rs`
- **Setup:** `mount` binary whitelisted.
- **Action:** `ostia run` with `mount -t tmpfs tmpfs /tmp`.
- **Expected:** stdout empty; stderr contains "Operation not permitted" or "permission denied"; exit non-zero.

### C-SC4: Process tracing is blocked
- **Test:** `seccomp_blocks_ptrace` in `crates/ostia-cli/tests/seccomp.rs`
- **Setup:** A minimal static binary that calls `ptrace(PTRACE_TRACEME)` is compiled and run in the sandbox.
- **Action:** `ostia run` with the ptrace test binary.
- **Expected:** stdout does NOT contain `ptrace succeeded`; stderr contains `EPERM` or "Operation not permitted"; exit non-zero.

## Test contracts — streaming output

### C-ST1: Real-time output preserves order
- **Test:** `streaming_cli_output_preserves_order` in `crates/ostia-cli/tests/streaming.rs`
- **Setup:** Default sandbox with baseline.
- **Action:** `ostia run` with `echo first && sleep 0.2 && echo second`.
- **Expected:** stdout contains `first` and `second` in order; exit 0.

### C-ST2: stderr streams separately from stdout
- **Test:** `streaming_stderr_stays_separate` in `crates/ostia-cli/tests/streaming.rs`
- **Setup:** Default sandbox.
- **Action:** `ostia run` with `echo out && bash -c 'echo err >&2'`.
- **Expected:** stdout `out`; stderr `err` (or empty stderr on the test harness depending on buffering semantics). No cross-contamination; exit 0.

### C-ST3: Tracing does not leak into stdout
- **Test:** `tracing_does_not_leak_into_stdout` in `crates/ostia-cli/tests/streaming.rs`
- **Setup:** `RUST_LOG=debug` set on the parent `ostia run` invocation.
- **Action:** `ostia run` with `echo clean`.
- **Expected:** stdout `clean` — no DEBUG/INFO/TRACE lines from the tracing subscriber; no framework noise; exit 0.

### C-SA5: Streaming channel delivers chunks
- **Test:** `streaming_channel_receives_chunks` in `crates/ostia-sandbox/tests/streaming_api.rs`
- **Setup:** `SandboxExecutor::execute_streaming` called programmatically.
- **Action:** Execute `echo one && echo two && echo three`.
- **Expected:** At least 3 `Stdout` chunks arrive on the channel, each containing its line; an `Exit(0)` event arrives to close the stream.

### C-SA6: Streaming tags stderr separately
- **Test:** `streaming_stderr_chunks_tagged_separately` in `crates/ostia-sandbox/tests/streaming_api.rs`
- **Setup:** `execute_streaming` called programmatically.
- **Action:** Execute `echo out && bash -c 'echo err >&2'`.
- **Expected:** `Stdout(data)` events contain `out`; `Stderr(data)` events contain `err`. No cross-contamination.

### C-SA7: `execute_streaming_collect` wraps the channel into `ExecutionResult`
- **Test:** `streaming_collect_into_execution_result` in `crates/ostia-sandbox/tests/streaming_api.rs`
- **Setup:** `execute_streaming_collect` called programmatically.
- **Action:** Execute `echo collected`.
- **Expected:** `result.allowed == true`; `result.exit_code == 0`; `result.stdout == "collected"`; `result.stderr` empty.

### C-SA8: Long-running command streams before exit
- **Test:** `streaming_first_chunk_arrives_before_exit` in `crates/ostia-sandbox/tests/streaming_api.rs`
- **Setup:** `execute_streaming` with a command that sleeps between prints.
- **Action:** Execute `echo first && sleep 0.3 && echo second && sleep 0.3 && echo third`.
- **Expected:** First stdout chunk arrives within 500ms of invocation; `Exit` event arrives 400ms+ later. Proves the runtime streams rather than buffering until completion.

## Test contracts — Docker packaging

### C-D25: Docker image builds
- **Test:** `docker_image_builds_successfully` in `crates/ostia-cli/tests/docker.rs`
- **Setup:** Project root with `Dockerfile`.
- **Action:** `docker build -t ostia:test .`.
- **Expected:** Build exits 0; image exists via `docker image inspect`.

### C-D26: Docker MCP handshake over HTTP
- **Test:** `docker_mcp_handshake_over_http` in `crates/ostia-cli/tests/docker.rs`
- **Setup:** Start container with HTTP transport bound to a host port.
- **Action:** Send JSON-RPC `initialize` to the container.
- **Expected:** Response includes `protocolVersion`; `serverInfo.name` contains `ostia`.

### C-D27: Docker sandboxed command executes
- **Test:** `docker_sandboxed_command_executes` in `crates/ostia-cli/tests/docker.rs`
- **Setup:** Container running HTTP MCP with the `dev` profile.
- **Action:** `tools/call` with `name=dev`, `command="echo docker-sandbox-works"`.
- **Expected:** `isError` absent/false; output contains `docker-sandbox-works`.

### C-D28: Docker image ships with common CLIs
- **Test:** `docker_cli_tools_available` in `crates/ostia-cli/tests/docker.rs`
- **Setup:** Container running; client handshakes.
- **Action:** `tools/call` for `git --version`, `curl --version`, `jq --version` in turn.
- **Expected:** Each call returns non-error with non-empty output. The built image includes git, curl, and jq out of the box.

### C-D29: Docker denies binaries not in the profile
- **Test:** `docker_denied_command_returns_error` in `crates/ostia-cli/tests/docker.rs`
- **Setup:** The `dev` profile does not include `python3`.
- **Action:** `tools/call` with `command="python3 -c 'print(1)'"`.
- **Expected:** `isError=true`.

### C-D30: Docker workspace volume mount
- **Test:** `docker_workspace_volume_mount` in `crates/ostia-cli/tests/docker.rs`
- **Setup:** Container with a host directory mounted as `/workspace:ro`; `<ws_path>/input.txt` on the host contains `host-file-content`.
- **Action:** `tools/call` with `command="cat /workspace/input.txt"`.
- **Expected:** Output contains `host-file-content`.

## Invariants (untested but load-bearing)

- **Base64/encoded bypasses cannot reach unlisted binaries.** Even `echo Y3VybA== | base64 -d | sh` cannot invoke `curl` if `curl` is not in the profile — the binary does not exist in the namespace.
- **Shell indirection cannot reach unlisted binaries.** `eval`, `source`, `exec`, `$()` all fail the same way: the target binary is not on the filesystem view.
- **Mount namespace setup runs inside `spawn_blocking`**, since it involves syscalls that must not be interleaved with tokio's IO driver on the same thread.
- **Landlock is best-effort on kernel < 5.13.** Ostia warns and operates with mount-namespace isolation only. (Tested behavior: see `context/sandbox-internals.md` — no integration test for the degraded path exists yet.)

## Non-goals

- PID namespace — requires Ostia to act as pid 1 (zombie reaper) inside the sandbox. Deferred past v1.
- User namespace — uid/gid mapping is disabled on some distros (Ubuntu 24.04 AppArmor restriction); mount namespaces work without it on most modern kernels.
- Flag-level parsing of commands — subcommand gating uses glob patterns against the full command string, not a CLI grammar parser.
