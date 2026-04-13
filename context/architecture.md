# Architecture

Ostia is a single Rust binary with a clean split between core data/config logic, OS-level sandboxing, and the CLI surface. The workspace is a Cargo workspace with three crates:

```
crates/
  ostia-core/      # Config, bundle resolution, credential providers, tool descriptions
  ostia-sandbox/   # unshare/mount/pivot_root, Landlock, seccomp, streaming execution
  ostia-cli/       # clap entry point, MCP server (stdio + HTTP), serve/check/run subcommands
```

## Crate boundaries — what goes where, why

### `ostia-core`
- Config parsing (`serde_yaml` → typed structs)
- Bundle composition, built-in bundles (`builtins.rs`, embedded via `include_str!`)
- Profile resolution: merging bundles, applying denies, validating references
- Credential provider framework: `command`/`env`/`file`/`http` fetch logic
- Identity resolution helpers (for `{{ user_id }}` templates)
- Tool description builder (`build_tool_description`) — shared by stdio and HTTP MCP handlers

Why: core is pure data and pure fetch logic. It owns nothing that requires Linux namespaces or privileged syscalls, which means it's testable on macOS and tests run without `assert_user_namespaces()` guards.

### `ostia-sandbox`
- Mount namespace construction: `unshare(CLONE_NEWNS)`, tmpfs root, binary + lib bind mounts, `pivot_root`
- Shared library resolution via `goblin` (recursive ELF dep walk)
- Landlock ruleset construction and enforcement
- Seccomp BPF filter generation via `seccompiler`
- Streaming execution: `execute_streaming` + `execute_streaming_collect` APIs
- `SandboxExecutor::from_profile(profile)` is the entry point

Why: sandbox owns everything Linux-specific. Users who only need core (e.g., validating config in CI on macOS) can depend on `ostia-core` without pulling in the kernel-syscall machinery. The crate split enforces that isolation.

### `ostia-cli`
- clap command definitions (`serve`, `check`, `run`)
- JSON-RPC MCP server for stdio and HTTP (via `axum`)
- `McpServer` state struct, `handle_request` dispatch
- Transport-specific glue: stdio line-reader, axum route handlers, endpoint filter

Why: the CLI is intentionally thin. It wires `ostia-core`'s resolved profile to `ostia-sandbox`'s executor and exposes both to the outside world via MCP. New interfaces (napi-rs Node.js bindings, PyO3 Python bindings) would be additional sibling crates that consume `ostia-core` + `ostia-sandbox` directly, not via `ostia-cli`.

## Cross-crate data flow

```
┌─ YAML file ───────────────────────────────────────────┐
│                          │                            │
│                          ▼                            │
│               ostia-core::OstiaConfig                 │
│                          │                            │
│          resolve_profile_with_identity(name, user_id) │
│                          │                            │
│                          ▼                            │
│                ostia-core::Profile                    │
│  (binaries, subcmd patterns, fs paths, env, creds)    │
│                          │                            │
│                          ▼                            │
│       ostia-sandbox::SandboxExecutor::from_profile    │
│                          │                            │
│                          ▼                            │
│       execute(cmd)  or  execute_streaming(cmd, tx)    │
└───────────────────────────────────────────────────────┘
```

The `Profile` type is the contract between `ostia-core` and `ostia-sandbox`. Anything a profile needs to express to the sandbox — binaries, subcommand allows/denies, filesystem paths, resolved credentials — must be a field on `Profile`. Adding a new sandbox capability means: extend the config schema in `core`, extend `Profile`, extend the sandbox to consume the new field.

## Why a single workspace binary

Because security-critical namespace setup must happen in a child process that has no dependency on heavyweight runtimes. Spawning a sub-binary to do the sandbox setup is the pattern; the child is `ostia` itself re-executed with a special mode flag, which keeps the binary count at one. Users install one thing.

## Why Rust

- Safe bindings to `unshare`, `mount`, `pivot_root`, `clone`, `ptrace` via `nix` and `libc`
- `landlock` and `seccompiler` crates are pure Rust, no libseccomp/libcap runtime dependency
- `goblin` for ELF dep resolution without subprocessing `ldd`
- Zero runtime dependencies — single static binary, no Python, no Node, no glibc-version surprises on user hosts

## Rust dependency choices

See `Cargo.toml` workspace-level deps for current versions. Key choices and why:

- **`nix` (v0.31)** — safe namespace/mount/unistd syscall bindings. Maintained, type-safe, covers everything we need.
- **`goblin` (v0.10)** — ELF parsing for shared library resolution. Pure Rust, no `ldd` subprocess dependency.
- **`landlock` (v0.4)** — safe Landlock LSM abstraction. Best-effort compat mode on older kernels.
- **`seccompiler` (v0.5)** — seccomp BPF filter construction. No libseccomp dependency.
- **`axum` + `tokio`** — HTTP transport for MCP. The server is a thin JSON-RPC handler over `axum::Router`.
- **`serde_yaml` (v0.9)** — config parsing. YAML with serde is the obvious choice.
- **`aes-gcm` + `base64`** — optional token mode for profile name encryption in HTTP transport. Disabled by default.

## Non-goals for the current layout

- Not splitting `ostia-sandbox` into sub-crates (one per enforcement layer). The mount-namespace, Landlock, and seccomp code is tightly sequenced in one execution path and doesn't benefit from artificial seams.
- Not adding an `ostia-mcp` crate separate from `ostia-cli`. The MCP server is ~300 lines of JSON-RPC glue on top of `SandboxExecutor` and doesn't need its own crate.
- Not vendoring bundles outside `ostia-core`. Built-in bundles are compiled into the binary via `include_str!` so a fresh install has no external file dependencies.
