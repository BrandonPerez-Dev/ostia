# Competitive Landscape: AI Agent Sandboxed Execution

> Date: 2026-03-31
> Status: research complete

## Summary

Ostia is differentiated. The specific combination of mount-namespace binary isolation + subcommand gating + auth gating + per-agent composable profiles + zero-dep Rust binary is unoccupied. The space is active but fragmented.

## Application-Level Tools (Not Real Competition)

These use no kernel primitives — a determined agent bypasses them trivially.

| Tool | Language | Security Model | Notes |
|------|----------|----------------|-------|
| mcp-shell (sonirico) | Go | Binary allowlist, shell parsing disabled in "secure" mode | No FS/network isolation |
| bash-mcp / mcp-server-bash | Various | None | README says "dev only" |
| mcp-shell-server (tumf) | Python | `ALLOW_COMMANDS` env var | Whitelisted binary has full host access |
| shell-commands-mcp (hdresearch) | Node.js | Hard-coded blocklist (rm, chmod, sudo) | Bypassable by name variation |

## OS-Level Competitors

### Greywall (GreyhavenHQ) — Closest competitor
- **Stack:** Go, Bubblewrap + Landlock + Seccomp BPF + eBPF + TUN proxy (Linux); Seatbelt (macOS)
- **Stars:** 118, active (v0.2.8, March 2026)
- **Gaps vs Ostia:** No binary-namespace isolation (uses path restrictions, not mount-based existence control). No subcommand gating. No auth gating. No Node.js bindings (Go library only).
- **Risk:** Could add binary-namespace features in 1-2 releases. Go-only is a distribution disadvantage in the TS/Node.js agent ecosystem.

### Nono (always-further) — Similar primitives
- **Stack:** Rust, Landlock (Linux) + Seatbelt (macOS), capability-based
- **Gaps vs Ostia:** No binary-namespace isolation. No subcommand gating. No auth gating. Has TypeScript FFI.
- **Risk:** Rust-based like Ostia. Positioned as developer security tool, not agent execution layer.

### Landrun (Zouuup) — General-purpose Landlock wrapper
- **Stack:** Go CLI, Landlock + TCP network restrictions
- **Stars:** 2,156
- **Gaps vs Ostia:** Not agent-aware. No subcommand gating, no auth gating, no profiles.

### ai-sandbox-landlock (classx)
- **Stack:** Rust, Landlock only, YAML profiles
- **Gaps vs Ostia:** Filesystem-only. No network proxy, no auth gating, no subcommand gating.

### AgentSH Secure Sandbox (canyonroad)
- **Stack:** Landlock + network proxy + shell shim + optional seccomp/FUSE
- **Gaps vs Ostia:** Policy preset-focused. Shell shim approach bypassable. No binary-existence isolation.

### Agent Safehouse (eugene1g)
- **Stack:** macOS Seatbelt only, shell script
- **Gaps:** macOS-only. Explicitly "not a perfect boundary."

### MCP Jail (mcpjail.com)
- **Stack:** Docker containers + seccomp + protocol proxy (Rust)
- **Gaps:** Targets MCP servers specifically, not the execute layer. Container-level isolation.

### Sandlock (multikernel.io)
- **Stack:** Python, Landlock + seccomp BPF + seccomp user notification
- **Gaps:** Python-only. COW fork() sandbox model. No agent-specific features.

## Agent Runtime Built-in Sandboxes

| Runtime | Mechanism | Binary Whitelisting? | Subcommand Gating? | Auth? | Standalone? |
|---------|-----------|---------------------|--------------------|----|-----------|
| Claude Code (`@anthropic-ai/sandbox-runtime`) | Bubblewrap/Seatbelt, FS allow/deny, network proxy | No | No | No | Yes (npm), requires bubblewrap |
| OpenAI Codex (linux-sandbox) | Bubblewrap + seccomp BPF + Landlock fallback | No | No | No | No (internal) |
| Cursor agent sandbox | Landlock + seccomp + Seatbelt, overlay FS | No | No | No | No (internal) |

## Cloud/Infrastructure Sandboxes

| Tool | Mechanism | Notes |
|------|-----------|-------|
| E2B | Firecracker microVMs (KVM) | Cloud-first, full VM per session, 15M sessions/month |
| Daytona | Docker (default), Kata/Sysbox optional | Pivoted to AI Feb 2025. Default isolation weak. |
| Microsandbox (zerocore-ai) | libkrun microVMs | Open source, MCP integration, experimental |
| OpenSandbox (Alibaba) | Docker + optional gVisor/Kata/Firecracker | Multi-language SDKs |
| Agent Sandbox (k8s-sigs) | gVisor + Kata on Kubernetes | Formal k8s subproject (Dec 2025) |

## Ostia's Unique Gaps Filled

### Gap 1: Binary-existence enforcement via mount namespace
Every competitor does filesystem path restrictions. Ostia constructs a namespace where unlisted binaries literally don't exist (pivot_root + selective bind mounts + dependency resolution). Can't bypass with symlinks, encoding tricks, or `/proc/self/exe`. No other tool does this.

### Gap 2: Subcommand-level allow/deny patterns
`gh pr list *` allowed, `gh pr merge *` denied. No tool in the landscape supports this granularity. Closest: Greywall's hard-coded dangerous-command blocklist.

### Gap 3: Auth gating before execution
Zero tools check whether a CLI tool is authenticated before execution. Agents waste turns on opaque errors from unauthenticated tools. Ostia declares `gh auth status` as a precondition and fails clearly.

### Gap 4: Per-agent composable profiles
Profiles compose from named bundles. Different agents get different capabilities. Most tools sandbox uniformly. Nono has preset profiles for specific agents (Claude Code, Codex) but they're vendor-defined, not user-composable.

### Gap 5: Zero-dep native binary with planned Node.js bindings
Rust core, no external dependencies (no bubblewrap, no socat). napi-rs bindings (V8) will make it the only npm-installable package wrapping actual namespace isolation.

## Risk Assessment

- **Moat strength:** Binary-namespace isolation is architecturally distinct — not a feature flag. This is the strongest differentiator.
- **Moat weakness:** Subcommand gating and auth gating are feature-level additions any competitor could ship in weeks.
- **Convergence risk:** If Anthropic's `sandbox-runtime` adds binary whitelisting and profiles, the gap narrows significantly.
- **Market risk:** If every major agent runtime ships its own sandbox (Claude Code, Codex, Cursor), demand for standalone execution layers shrinks.

## Sources

- sonirico/mcp-shell, tumf/mcp-shell-server, hdresearch/mcp-shell on GitHub
- Claude Code sandboxing docs (code.claude.com/docs/en/sandboxing)
- Anthropic engineering blog: Claude Code sandboxing
- @anthropic-ai/sandbox-runtime on npm
- OpenAI Codex linux-sandbox README
- Cursor agent sandboxing blog post
- GreyhavenHQ/greywall on GitHub + greywall.io
- always-further/nono on GitHub
- Zouuup/landrun on GitHub
- mcpjail.com
- E2B docs, Daytona, microsandbox, OpenSandbox, k8s-sigs/agent-sandbox
- Pierce Freeman: "A deep dive on agent sandboxes"
- Ry Walker: "Local AI Agent Sandboxes Compared"
