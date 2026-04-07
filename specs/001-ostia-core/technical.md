# Ostia — Technical Specification

**Date:** 2026-03-23
**Status:** Draft
**Spec:** [spec.md](spec.md)

## Architecture Overview

Ostia is a single Rust binary with three interfaces: native Node.js module (napi-rs), MCP server (stdio + HTTP), and CLI. All three share the same core: config parsing, dependency resolution, namespace construction, and sandboxed command execution.

```
┌─────────────────────────────────────────────────────┐
│                    Interfaces                        │
│  ┌──────────┐  ┌──────────────┐  ┌───────────────┐  │
│  │ napi-rs  │  │  MCP Server  │  │     CLI       │  │
│  │ (Node.js)│  │ (stdio/HTTP) │  │ (serve/check) │  │
│  └────┬─────┘  └──────┬───────┘  └───────┬───────┘  │
│       └───────────┬────┘─────────────────┘           │
│                   ▼                                  │
│  ┌─────────────────────────────────────────────────┐ │
│  │              Core Engine                         │ │
│  │  ┌─────────┐ ┌──────────┐ ┌───────────────────┐ │ │
│  │  │ Config  │ │ Command  │ │   Auth Checker    │ │ │
│  │  │ Parser  │ │ Matcher  │ │                   │ │ │
│  │  └────┬────┘ └────┬─────┘ └────────┬──────────┘ │ │
│  │       └──────┬────┘───────────────┘              │ │
│  │              ▼                                   │ │
│  │  ┌─────────────────────────────────────────────┐ │ │
│  │  │           Sandbox Engine                     │ │ │
│  │  │  ┌────────────┐ ┌──────────┐ ┌───────────┐  │ │ │
│  │  │  │ Dep Resolve│ │Namespace │ │  Network   │  │ │ │
│  │  │  │  (goblin)  │ │ Builder  │ │   Proxy    │  │ │ │
│  │  │  └────────────┘ └──────────┘ └───────────┘  │ │ │
│  │  │  ┌────────────┐ ┌──────────┐                │ │ │
│  │  │  │  Landlock   │ │ Seccomp  │                │ │ │
│  │  │  │  Enforcer   │ │  Filter  │                │ │ │
│  │  │  └────────────┘ └──────────┘                │ │ │
│  │  └─────────────────────────────────────────────┘ │ │
│  └─────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────┘
```

## Crate Workspace Structure

```
ostia/
  Cargo.toml              # workspace root
  crates/
    ostia-core/            # Config, matching, auth checking
      src/
        lib.rs
        config.rs          # YAML parsing, profile/bundle resolution
        matcher.rs         # Glob pattern matching for subcommands
        auth.rs            # Auth status checking (subprocess runner)
        description.rs     # Dynamic tool description generator
    ostia-sandbox/         # OS-level isolation
      src/
        lib.rs
        resolve.rs         # ELF dependency resolution (goblin)
        namespace.rs       # Mount/network/PID namespace setup
        mounts.rs          # Bind mount choreography, pivot_root
        landlock.rs        # Landlock ruleset construction
        seccomp.rs         # seccomp BPF filter generation
        proxy.rs           # HTTP/SOCKS proxy for network filtering
    ostia-node/            # napi-rs bindings
      src/
        lib.rs
      package.json
    ostia-mcp/             # MCP server (stdio + HTTP)
      src/
        lib.rs
        stdio.rs
        http.rs
        tools.rs           # execute, status, list_tools definitions
    ostia-cli/             # CLI binary
      src/
        main.rs            # serve, check, run subcommands
  bundles/                 # Built-in bundle definitions
    baseline.yaml
    git-read.yaml
    git-write.yaml
    github-read.yaml
    github-rw.yaml
    k8s-read.yaml
    docker.yaml
  tests/
    integration/           # End-to-end tests (require Linux namespaces)
```

## Key Design Decisions

### 1. Namespace Strategy

Use `unshare(CLONE_NEWNS | CLONE_NEWNET)` in a forked child process. The child sets up the sandbox, then `exec`s the target command. The parent waits and captures stdout/stderr.

**Not using CLONE_NEWPID** in v1 — PID namespace requires ostia to act as PID 1 (zombie reaper) inside the namespace, adding complexity. Mount + network isolation is sufficient for our threat model.

**Not using CLONE_NEWUSER** in v1 — user namespaces add uid/gid mapping complexity and are disabled on some distros (Ubuntu 24.04 restricted them). Mount namespaces work without user namespaces on most modern kernels when unprivileged user namespaces are enabled.

### 2. Mount Choreography

The sequence per command execution:

```
1. fork()
   In child:
2. unshare(CLONE_NEWNS | CLONE_NEWNET)
3. mount(NULL, "/", NULL, MS_REC | MS_PRIVATE, NULL)      // kill propagation
4. mount("tmpfs", NEWROOT, "tmpfs", MS_NOSUID|MS_NODEV, "size=64m")
5. For each whitelisted binary:
   a. Resolve binary path (which <name>)
   b. Resolve all shared library deps via goblin (recursive)
   c. Create target dirs on tmpfs
   d. Bind-mount binary (read-only, two-step)
   e. Bind-mount all resolved libraries (read-only, two-step)
   f. Bind-mount the ELF interpreter
6. Create library path symlinks (/lib -> /usr/lib, /lib64 -> /usr/lib64, etc.)
7. Bind-mount /bin/sh (always — needed to run commands)
8. Mount fresh /proc (MS_NOSUID | MS_NOEXEC | MS_NODEV)
9. Bind-mount minimal /dev: null, zero, urandom, random, full
10. Bind-mount workspace directory (read-write at configured path)
11. Bind-mount additional read paths (read-only)
12. Bind-mount Unix socket for network proxy
13. pivot_root(NEWROOT, NEWROOT/oldroot) via libc::syscall
14. chdir("/")
15. umount2("/oldroot", MNT_DETACH) + rmdir
16. prctl(PR_SET_NO_NEW_PRIVS, 1)
17. Apply Landlock ruleset
18. Apply seccomp filter
19. Set HTTP_PROXY / HTTPS_PROXY / ALL_PROXY env vars → Unix socket path
20. exec("/bin/sh", ["-c", command])
```

### 3. Dependency Resolution (goblin)

At profile load time (not per-command):

```rust
fn resolve_all_deps(binaries: &[&str]) -> HashMap<PathBuf, HashSet<PathBuf>> {
    // For each binary:
    // 1. which(binary) -> /usr/bin/gh
    // 2. goblin::elf::Elf::parse -> .libraries, .interpreter, .runpaths
    // 3. Resolve each soname to absolute path (ldconfig -p cache or default paths)
    // 4. Recurse on each resolved library
    // 5. Return: binary -> {binary_path, interpreter, lib1, lib2, ...}
}
```

Cache resolved deps per profile. Only re-resolve when config changes.

### 4. Subcommand Matching

Before entering the namespace, validate the command string:

1. Split compound commands on `&&`, `||`, `;`, `|` (shell-aware splitting, respecting quotes)
2. For each subcommand, extract the binary name and arguments
3. Match against the profile's subcommand patterns using glob matching
4. If any subcommand fails validation, reject the entire command

Pattern matching uses `glob::Pattern` or equivalent. Patterns like `gh pr list *` match the full command string after the binary name.

### 5. Network Proxy

A lightweight HTTP/HTTPS + SOCKS5 proxy running in the host namespace:

- Listens on a Unix socket
- Socket file is bind-mounted into the sandbox
- Sandbox sets `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY` env vars pointing to the socket
- Proxy validates each connection against the profile's domain allowlist
- Blocked connections return a clear error (not a timeout)

The proxy starts once per ostia instance and serves all command executions for that profile.

Implementation: `hyper` for HTTP, `tokio` for async I/O. The proxy is ~500-800 lines.

### 6. Config Format

```yaml
# ostia.yaml

bundles:
  baseline:
    binaries: [cat, grep, ls, find, head, tail, jq, wc, sed, awk, echo, date, whoami]

  git-read:
    binaries: [git]
    subcommands:
      - git log *
      - git diff *
      - git status
      - git branch -l
      - git show *

  github-rw:
    binaries: [gh]
    subcommands:
      - gh pr list *
      - gh pr view *
      - gh pr create *
      - gh issue list *
      - gh issue view *
      - gh issue create *
    auth:
      github:
        check: gh auth status
        hint: "Run 'gh auth login' to authenticate"

profiles:
  build-agent:
    bundles: [baseline, git-read, github-rw]
    # Additional tools beyond bundles
    tools:
      binaries: [npm, node]
      subcommands:
        - npm test *
        - npm run *
        - npm install *
    # Profile-level denies (override bundle allows)
    deny:
      - gh repo delete *
      - npm publish *
    filesystem:
      workspace: /app/project        # read-write
      read:
        - /usr
        - /etc/ssl
      deny_read:
        - ~/.ssh
        - ~/.aws
        - ~/.config
        - .env
      deny_write:
        - .git/hooks
        - .bashrc
        - .zshrc
    network:
      allow:
        - github.com
        - "*.github.com"
        - registry.npmjs.org
        - nodejs.org

  planning-agent:
    bundles: [baseline, git-read]
    tools:
      binaries: [gh]
      subcommands:
        - gh pr list *
        - gh pr view *
        - gh issue list *
        - gh issue view *
        # No write operations
    filesystem:
      workspace: /app/project
    network:
      allow:
        - github.com
        - "*.github.com"
```

### 7. API Surface

**Node.js (napi-rs):**

```typescript
import { Ostia } from 'ostia';

const ostia = new Ostia({
  configPath: './ostia.yaml',
  profile: 'build-agent',
});

// Check what's available
const status = ostia.status();
// { tools: [...], auth: { github: 'active', npm: 'inactive' }, ... }

const tools = ostia.availableTools();
// "Available: gh pr *, npm test *, ... | Auth: github=active, npm=inactive"

// Execute a command
const result = ostia.execute('gh pr list --repo foo/bar');
// { stdout: '...', stderr: '...', exitCode: 0, command: 'gh pr list ...', allowed: true }

// Rejected command
const result2 = ostia.execute('curl http://evil.com');
// { stdout: '', stderr: 'binary not found: curl', exitCode: 127, allowed: false,
//   reason: 'curl is not whitelisted in profile build-agent' }
```

**MCP Tools:**

```json
{
  "tools": [
    {
      "name": "execute",
      "description": "Run a whitelisted CLI command.\nProfile: build-agent | Mode: read-write\n\nAvailable: gh pr *, npm test *, git log *, ...\nAuth: github=active, npm=inactive\n\nBaseline: cat, grep, ls, find, head, tail, jq, wc, sed, awk",
      "inputSchema": {
        "type": "object",
        "required": ["command"],
        "properties": {
          "command": { "type": "string", "description": "The CLI command to execute" },
          "working_dir": { "type": "string", "description": "Working directory (must be within workspace)" },
          "timeout_ms": { "type": "integer", "description": "Timeout in milliseconds (default: 30000)" }
        }
      }
    },
    {
      "name": "status",
      "description": "Check available tools and auth status",
      "inputSchema": { "type": "object", "properties": {} }
    },
    {
      "name": "list_tools",
      "description": "List all available CLI commands and their subcommand patterns",
      "inputSchema": { "type": "object", "properties": {} }
    }
  ]
}
```

## Vertical Slices

### Slice 0: Walking Skeleton
**Goal:** A Rust binary that loads a YAML config, resolves one whitelisted binary, creates a mount namespace with only that binary visible, and executes a command inside it.

- Config parser (minimal — one profile, one binary, no bundles)
- Dependency resolver (goblin — resolve one binary's libs)
- Namespace builder (unshare + mount + pivot_root)
- Execute a command and return stdout/stderr
- Integration test: whitelist `echo`, verify `echo hello` works, verify `curl` fails with "not found"

### Slice 1: Full Config & Bundles
- Complete YAML config parsing with profiles, bundles, subcommands, deny rules
- Bundle composition with additive merging and deny overrides
- Config validation with clear error messages
- Unit tests for config parsing and bundle resolution

### Slice 2: Subcommand Matching
- Shell-aware command splitting (&&, ||, ;, |, respecting quotes)
- Glob pattern matching against subcommand whitelist
- Compound command validation (all subcommands must pass)
- Unit tests for pattern matching edge cases

### Slice 3: Filesystem Isolation
- Landlock ruleset construction from profile config
- Mandatory deny paths (always blocked regardless of config)
- Read/write path configuration
- Integration tests: verify writes outside workspace fail, verify sensitive paths are blocked

### Slice 4: Network Isolation
- Network namespace (CLONE_NEWNET)
- HTTP/SOCKS proxy with domain allowlist
- Unix socket relay between namespaces
- Proxy env var injection
- Integration tests: verify allowed domains work, blocked domains fail

### Slice 5: Auth Status
- Auth check command execution at init
- Status reporting (active/inactive per service)
- Dynamic tool description generation with auth status
- Pre-execution auth validation with clear errors

### Slice 6: napi-rs Bindings
- Node.js native module wrapping ostia-core
- Ostia class with constructor, execute, status, availableTools
- npm package configuration
- Integration tests from Node.js

### Slice 7: MCP Server
- stdio transport (JSON-RPC over stdin/stdout)
- HTTP transport (streamable HTTP)
- execute, status, list_tools tool definitions
- Profile locked at server start
- Integration tests with MCP client

### Slice 8: CLI Binary
- `ostia serve --profile <name> --transport stdio|http [--port N]`
- `ostia check --profile <name>` — validate config, check auth, report status
- `ostia run --profile <name> -- <command>` — one-off sandboxed execution

### Slice 9: Seccomp Hardening
- seccomp BPF filter: deny mount, unshare, clone (with ns flags), ptrace, kexec_load
- prctl(PR_SET_NO_NEW_PRIVS) before exec
- Integration tests: verify blocked syscalls fail inside sandbox

### Slice 10: Built-in Bundles & Polish
- Ship baseline, git-read, git-write, github-read, github-rw, k8s-read, docker bundles
- Error message polish
- Graceful degradation (Landlock unavailable, user namespaces disabled, binary not found)
- README and usage docs

## Open Technical Questions (from spec)

1. **Shared library resolution strategy:** Use `goblin` to parse ELF `DT_NEEDED`, resolve sonames via `ldconfig -p` output parsing (subprocess), then recursive BFS. Cache results per profile.

2. **Stateful tools:** Mount tool-specific state directories as read-write inside the namespace. E.g., `.git/` is writable if `git` is whitelisted. Define in config per-tool or per-profile.

3. **Shell selection:** Always mount `/bin/sh`. If the profile specifies a different shell, mount that too. Execute commands via `sh -c "<command>"`.

4. **seccomp scope:** Yes, use seccomp as defense-in-depth. Block: `mount`, `unshare`, `clone` (with namespace flags), `ptrace`, `kexec_load`, `open_by_handle_at`. Allow everything else.

5. **Container environments:** Document minimum capabilities needed. `CAP_SYS_ADMIN` for nested namespaces inside Docker. Alternatively, use `--privileged` or specific seccomp profile that allows `unshare`.

## Rust Dependencies

```toml
[workspace.dependencies]
# Namespace / OS
nix = { version = "0.31", features = ["sched", "mount", "process", "signal", "socket", "unistd"] }
libc = "0.2"

# ELF parsing
goblin = "0.10"

# Security
landlock = "0.4"
seccompiler = "0.5"

# Config
serde = { version = "1", features = ["derive"] }
serde_yaml = "0.9"

# Pattern matching
glob = "0.3"

# Network proxy
hyper = { version = "1", features = ["server", "http1"] }
tokio = { version = "1", features = ["full"] }

# MCP server
# (evaluate: rmcp, mcp-rs, or hand-roll JSON-RPC)

# Node.js bindings
napi = "3"
napi-derive = "3"

# CLI
clap = { version = "4", features = ["derive"] }

# Error handling
thiserror = "2"
anyhow = "1"
```

## Security Boundaries

- **Always:** Deny execution of binaries not in the whitelist (mount namespace). Deny writes to mandatory deny paths. Block network to non-allowed domains. prctl(PR_SET_NO_NEW_PRIVS) before exec. Apply seccomp filter.
- **Ask first:** Adding new binaries to profiles. Enabling network access. Granting write access beyond workspace.
- **Never:** Allow profile selection by the model (orchestrator only). Mount host /proc or /dev wholesale. Allow setuid binaries inside the sandbox. Skip PR_SET_NO_NEW_PRIVS.
