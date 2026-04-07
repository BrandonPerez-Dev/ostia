# Plan: Ostia Phase 2 — Hardening + Interfaces

> Date: 2026-03-25
> Status: planning

## What & Why

Ostia's walking skeleton works — mount namespace isolation, config/bundle parsing, subcommand matching, and a CLI. Now we harden the sandbox (Landlock, seccomp, network isolation, filesystem policy) and add the production interfaces (auth status, MCP server, napi-rs bindings). Hardening first because the security story IS the product — without it, this is just a wrapper around `sh -c`.

## Constraints

- **Landlock via `landlock` crate (v0.4)** — kernel-level filesystem restrictions as defense-in-depth on top of mount namespace. Best-effort compat mode for kernels < 5.13.
- **seccomp via `seccompiler` crate (v0.5)** — block mount/unshare/clone(ns flags)/ptrace/kexec_load inside sandbox. Pure Rust, no libseccomp dependency.
- **Network isolation via CLONE_NEWNET + proxy** — agent process starts with zero network. HTTP/SOCKS proxy on host side, connected via Unix socket. `hyper` + `tokio` for the proxy.
- **Auth checks are executable commands** — `gh auth status` returns 0/1. Run at init, cache result, surface in tool description. No credential storage.
- **MCP server uses `rmcp` or hand-rolled JSON-RPC** — evaluate which Rust MCP crate is production-ready. stdio + streamable HTTP transports.
- **napi-rs v3** for Node.js bindings — single native addon, optionalDependencies pattern for platform variants.
- **Debug output must be removed** before any hardening work — `eprintln!("ostia-debug: ...")` lines in namespace.rs are leftover from Slice 0.

## Non-Goals

- PID namespace (v2 — requires zombie reaper)
- Credential injection into sandbox (v2)
- macOS native sandbox (Docker path only)
- Python bindings (future via PyO3)

## Verticals

### V1: Cleanup + debug output removal
- **Does:** Remove all `ostia-debug` eprintln statements, fix `unused has_bin_sh` warning
- **Done when:** `cargo build` produces zero warnings, `cargo test` passes, `ostia run` produces only command output
- **Test:** `cargo run -- run --config test-config.yaml --profile test -- echo hello 2>&1` outputs exactly `hello\n`
- **Deps:** None

### V2: Landlock filesystem enforcement
- **Does:** Apply Landlock ruleset after pivot_root to restrict filesystem access based on profile config (workspace rw, read paths ro, mandatory deny paths)
- **Done when:** A command that writes outside workspace fails. A command reading ~/.ssh fails. Workspace writes succeed.
- **Test:** Integration test: profile with workspace=/tmp/ostia-test, write to workspace succeeds, write to /tmp/other fails with EACCES, read of /etc/shadow fails
- **Deps:** V1

### V3: Seccomp hardening
- **Does:** Apply seccomp BPF filter before exec — blocks mount, unshare, clone(ns flags), ptrace, kexec_load, open_by_handle_at
- **Done when:** A sandboxed process cannot create new namespaces, mount filesystems, or ptrace other processes
- **Test:** Integration test: sandboxed command attempts `unshare --mount echo test`, fails with EPERM
- **Deps:** V1

### V4: Network namespace + proxy
- **Does:** Add CLONE_NEWNET to sandbox, run HTTP/SOCKS proxy on host side with domain allowlist, relay via Unix socket
- **Done when:** Allowed domains reachable, non-allowed domains blocked, no-network-config means all blocked
- **Test:** Integration test: profile allows `github.com`, `curl https://github.com` works inside sandbox, `curl https://evil.com` fails
- **Deps:** V1

### V5: Auth status checking
- **Does:** Run auth check commands at profile init, cache results, expose via `status()` and dynamic tool descriptions
- **Done when:** `ostia check` shows auth status per service. `execute()` returns clear "auth required" error for inactive services.
- **Test:** Unit test: mock auth check returning 0 → active, returning 1 → inactive. Integration: profile with `gh auth status` check, status output shows result.
- **Deps:** V1

### V6: Built-in bundles + config polish
- **Does:** Ship baseline, git-read, git-write, github-read, github-rw, k8s-read, docker bundles. Add graceful degradation for missing Landlock, disabled user namespaces, missing binaries.
- **Done when:** Profile can reference built-in bundles without defining them in config. Missing capabilities produce clear warnings.
- **Test:** Config that uses `bundles: [baseline, git-read]` without defining those bundles resolves correctly.
- **Deps:** V2, V3, V5

### V6.5: Streaming output
- **Does:** Replace batch `execute()` with streaming execution — forward stdout/stderr chunks to the caller as they arrive instead of buffering until command completes
- **Done when:** Agent runtimes receive output chunks in real-time during long-running commands. Rust API supports callback or channel-based streaming. CLI `ostia run` passes through stdio directly.
- **Test:** Integration test: long-running command produces output chunks before the command exits
- **Deps:** V1
- **Why early:** Agents need to display live output to users. A 30-second `cargo build` with no feedback is a dead screen. This is prerequisite for a usable agent tool interface and must land before MCP server (V7).

### V7: MCP server (stdio + HTTP) (headline — detail later)
### V8: napi-rs Node.js bindings (headline — detail later)
### V9: Integration tests + CI (headline — detail later)

### VR: Test quality revision (retroactive)
- **Does:** Fix test gaps and quality issues discovered during audit of V1-V6
- **Done when:** All tests use hard guards (no silent skipping), seccomp blocks are integration-tested, auth blocks execute(), failure assertions check error reasons, no redundant tests
- **Deps:** V1-V6

**Group A — Feature gaps (new tests):**
- A1: `seccomp_blocks_unshare` — `unshare --mount echo escaped` fails with EPERM inside sandbox
- A2: `seccomp_blocks_mount` — `mount -t tmpfs tmpfs /tmp` fails with EPERM inside sandbox
- A3: `run_with_inactive_auth_fails` — `ostia run` hard-fails when any auth check returns nonzero. Stderr contains "auth" + service name. Stdout empty (command never ran).
- A4: `run_with_active_auth_succeeds` — `ostia run` with `auth: { svc: { check: "true" } }` runs normally
- A5: `run_without_auth_config_succeeds` — backward compat, no auth section → no check

**Group B — Quality fixes (revise existing):**
- B1: Replace all silent `if !available() { return }` with hard assertions — no silent skipping in CI
- B2: Rename `read_nonexistent_sensitive_path_fails` → `sensitive_paths_not_visible_in_sandbox`, move to namespace test file
- B3: Landlock failure assertions must check stderr for "Permission denied" or "Read-only", not just exit code
- B4: `mandatory_deny_path_is_blocked` creates its own `.ssh` dir instead of depending on host `~/.ssh`
- B5: Delete redundant `combined_output_has_no_debug_lines`, replace with `failing_command_stderr_has_no_debug_lines`
- B6: Delete redundant `check_shows_auth_section_header`
- B7: Auth status assertions verify service+status appear on the same line
- B8: Clean up 50-line debugging journal in `jump_offsets_are_correct`

**Group C — Graceful degradation (new tests):**
- C1: `missing_binary_check_warns_without_crash` — `ostia check` with nonexistent binary exits 0, shows `[missing]`
- C2: `missing_binary_allows_other_commands` — `ostia run` with one missing binary still runs allowed commands

## Open Questions

1. **Which MCP Rust crate?** Need to evaluate `rmcp` vs hand-rolling JSON-RPC. The MCP protocol for tool servers is simple enough that hand-rolling might be less dependency debt.
2. **Network proxy complexity** — The HTTP/SOCKS proxy is the biggest single piece of new code (~500-800 lines). Could defer to a later phase if auth + MCP are higher priority.
3. ~~**Built-in bundle loading** — Embed as `include_str!` in the binary? Or look for a `bundles/` directory next to the config file?~~ Resolved: embedded as Rust constants in `builtins.rs`.
