[![CI](https://img.shields.io/badge/build-passing-brightgreen)]() [![License](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)

# Ostia

OS-level sandbox for AI agent tool calls. Run shell commands in isolated Linux namespaces with per-profile allow/deny controls, filesystem restrictions, and credential injection — served as an MCP server over stdio or HTTP.

## Quick Start

```bash
docker run -d \
  --name ostia \
  -p 8080:8080 \
  -v /path/to/workspace:/workspace \
  ghcr.io/brandonperez-dev/ostia:latest
```

Send an MCP `tools/list` request to see available profiles:

```bash
curl -s http://localhost:8080/mcp -d '{
  "jsonrpc": "2.0", "id": 1, "method": "tools/list"
}' | jq '.result.tools[].name'
```

```
"readonly"
"dev"
"node"
"python"
```

Execute a command in the `dev` profile:

```bash
curl -s http://localhost:8080/mcp -d '{
  "jsonrpc": "2.0", "id": 2,
  "method": "tools/call",
  "params": { "name": "dev", "command": "git log --oneline -5" }
}' | jq -r '.result.content[0].text'
```

## What Is Ostia?

Ostia is a sandboxed command execution engine designed for AI agents that need to run shell commands safely. It sits between the agent and the host OS, enforcing:

- **Which binaries** the agent can invoke (allowlists via bundles)
- **Which subcommands** are permitted or denied (glob patterns like `git push *`)
- **Which files** are visible and writable (bind-mount isolation)
- **Which credentials** are available (injected as env vars, never inherited)
- **Which network** access is allowed (Landlock-enforced)

Each profile is a different security posture. A `readonly` profile can browse code but not modify files. A `dev` profile has git and curl but blocks `git push`. A `node` profile adds npm/npx. Profiles are exposed as MCP tools — one tool per profile — so the agent sees exactly the capabilities available to it.

## What Ostia Is NOT

- **Not a container runtime.** Ostia uses Linux namespaces and Landlock for isolation, but it doesn't manage images, layers, or orchestration. It's closer to nsjail than Docker.
- **Not a secrets manager.** The credential system fetches secrets from existing sources (gcloud, gh, vault APIs, env vars, files) and injects them into the sandbox. It doesn't store or rotate secrets.
- **Not an agent framework.** Ostia provides the execution layer. It doesn't make decisions about what to run — the agent does that via MCP tool calls.

## Features

- **Linux namespace isolation** — mount, user, and PID namespaces. The sandbox sees only explicitly mounted binaries and their library dependencies.
- **Landlock filesystem enforcement** — kernel-level read/write restrictions beyond what mount namespaces provide.
- **Seccomp filtering** — restricts available system calls inside the sandbox.
- **Command matching** — glob-based allow/deny patterns. Allow `git log *` while denying `git push *` in the same profile.
- **Binary resolution** — automatically resolves shared library dependencies (via ELF parsing) and bind-mounts only what each binary needs.
- **Credential injection** — External Secrets Operator pattern. Supports `command`, `env`, `file`, and `http` providers with built-in presets for gcloud, GitHub CLI, and AWS.
- **Explicit environment** — sandbox processes get `PATH=/usr/bin:/bin`, `HOME=/`, `TERM=dumb` plus credential-injected vars. No host environment leakage.
- **Mandatory deny list** — `.ssh`, `.env`, `.aws`, `.gnupg`, `.config/gh`, and other sensitive paths are never mounted, regardless of profile config.
- **MCP protocol** — each profile becomes an MCP tool. Supports stdio and HTTP transports. Multi-profile endpoints for scoped access.
- **Streaming output** — command output streams in real-time, not buffered.

## Installation

### Docker (recommended)

```bash
docker run -d \
  --name ostia \
  -p 8080:8080 \
  -v /path/to/workspace:/workspace \
  -v /path/to/config.yaml:/etc/ostia/config.yaml \  #optional
  ghcr.io/brandonperez-dev/ostia:latest
```

```yaml
# docker-compose.yml
services:
  ostia:
    image: ghcr.io/brandonperez-dev/ostia:latest
    ports:
      - "8080:8080"
    volumes:
      - /path/to/workspace:/workspace
      - /path/to/config.yaml:/etc/ostia/config.yaml  #optional
    restart: unless-stopped
```

The default image includes: git, curl, wget, jq, Node.js, npm, Python 3, and pip.

### From Source

Requires Rust 1.85+ (edition 2024) and a Linux host:

```bash
git clone https://github.com/BrandonPerez-Dev/ostia
cd ostia
cargo build --release
# Binary at target/release/ostia
```

## Usage

### MCP Server (primary interface)

**Stdio transport** (for Claude Desktop, Claude Code, etc.):

```bash
ostia serve --config config.yaml --transport stdio
```

**HTTP transport** (for networked agents):

```bash
ostia serve --config config.yaml --transport http --host 0.0.0.0 --port 8080
```

### Claude Desktop Integration

Add to `~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "ostia": {
      "command": "ostia",
      "args": ["serve", "--config", "/path/to/config.yaml", "--transport", "stdio"]
    }
  }
}
```

Or connect to a running HTTP instance:

```json
{
  "mcpServers": {
    "ostia": {
      "url": "http://localhost:8080/mcp"
    }
  }
}
```

### Direct CLI

```bash
# Run a command in a specific profile
ostia run --config config.yaml --profile dev -- git status

# Validate a profile and check binary resolution
ostia check --config config.yaml --profile dev
```

## Configuration

Ostia is configured via a YAML file. Profiles compose bundles (groups of binaries) with deny patterns, filesystem rules, and credentials.

```yaml
bundles:
  baseline:
    binaries: [sh, bash, cat, ls, grep, sed, awk, find, sort, wc]

  dev-tools:
    description: "git, curl, wget, jq"
    binaries: [git, curl, wget, jq]

profiles:
  readonly:
    description: "Browse code without modifying files"
    bundles: [baseline]
    filesystem:
      workspace: /workspace
    deny:
      - rm *
      - mv *
      - cp *

  dev:
    description: "Development shell with git and curl"
    bundles: [baseline, dev-tools]
    filesystem:
      workspace: /workspace
    deny:
      - git push *
      - git remote *
    credentials:
      gcloud: preset
      github: preset
      vault-token:
        provider: http
        url: "https://vault.example.com/v1/secrets/{{ user_id }}"
        headers:
          X-Vault-Token: "{{ user_id }}"
        inject:
          VAULT_TOKEN: token
```

### Profiles

Each profile defines a security posture:

| Field | Description |
|-------|-------------|
| `bundles` | List of bundle names to include |
| `tools.binaries` | Additional binaries beyond bundles |
| `deny` | Glob patterns for denied subcommands |
| `filesystem.workspace` | Working directory inside the sandbox |
| `filesystem.read` | Additional read-only mount paths |
| `filesystem.deny_read` | Paths blocked from reading |
| `filesystem.deny_write` | Paths blocked from writing |
| `network.allow` | Allowed network destinations |
| `env` | Static environment variables |
| `credentials` | Credential providers (see below) |

### Credential Providers

Follows the [External Secrets Operator](https://external-secrets.io/) pattern. Each provider fetches a secret and the `inject` block maps provider output keys to sandbox environment variables.

| Provider | Source | Config Fields |
|----------|--------|---------------|
| `command` | Shell command stdout | `command`, `inject` |
| `env` | Host environment variable | `env`, `inject` |
| `file` | Host file contents | `path`, `inject` |
| `http` | JSON API response | `url`, `headers`, `inject` |

Built-in presets: `gcloud` (access token), `github` (gh auth token), `aws` (session credentials).

The `{{ user_id }}` template in `url` and `headers` fields is interpolated from `--user-id` flag or `OSTIA_USER_ID` environment variable.

### Endpoints

Group profiles into named endpoints for scoped MCP access:

```yaml
endpoints:
  safe:
    - readonly
    - dev
  full:
    - readonly
    - dev
    - node
    - python
```

Access via `POST /mcp/{endpoint_name}`. Requests to `/mcp/safe` only see the `readonly` and `dev` tools.

### Server Authentication

```yaml
auth:
  mode: token    # "open" (default) or "token"
  key: "base64-encoded-32-byte-AES-key"
```

In `token` mode, the profile name is encrypted in an AES-256-GCM token passed per request. In `open` mode, profile names are passed directly.

## MCP Tools

Each profile in the config becomes an MCP tool. The tool accepts a single `command` argument:

```json
{
  "name": "dev",
  "description": "Development shell with git and curl\nTools: git, curl, wget, jq\nDenied: git push *, git remote *\nWorkspace: /workspace",
  "inputSchema": {
    "type": "object",
    "properties": {
      "command": {
        "type": "string",
        "description": "The shell command to execute"
      }
    },
    "required": ["command"]
  }
}
```

Tool descriptions are auto-generated from the profile config: included bundles, deny patterns, and workspace path — so the agent knows what it can and can't do before calling.

## Use Cases

- **AI agent tool calls** — give Claude, GPT, or other agents safe shell access with per-profile guardrails
- **CI credential isolation** — inject credentials into build steps without exposing them to the full environment
- **Multi-tenant dev environments** — each developer or team gets a profile with different access levels
- **Code review sandboxing** — readonly profiles for automated code analysis without modification risk
- **Secure API proxying** — use HTTP provider credentials to give agents access to vault/secrets APIs through scoped env vars

## Security

Ostia enforces isolation at multiple layers:

| Layer | Mechanism | What It Does |
|-------|-----------|-------------|
| **Namespace** | Mount, user, PID | Isolated filesystem view, unprivileged user, separate process tree |
| **Landlock** | LSM | Kernel-enforced read/write path restrictions |
| **Seccomp** | BPF filter | Restricts available syscalls |
| **Command matching** | Glob patterns | Allow/deny specific subcommands |
| **Mandatory deny** | Hardcoded paths | `.ssh`, `.env`, `.aws`, `.gnupg`, `.config/gh` never mounted |
| **Explicit env** | execve | No host environment inheritance; only baseline + injected vars |

**Limitations:**
- Requires Linux (namespaces and Landlock are Linux-specific)
- Landlock enforcement depends on kernel version (5.13+ for filesystem, 6.8+ for network)
- The sandbox runs as an unprivileged user inside its namespace, but Ostia itself needs to run as a user with permission to create user namespaces

## Contributing

Contributions are welcome. Please open an issue before submitting large PRs.

```bash
# Development
cargo build
cargo test

# Run tests (requires Linux)
cargo test --workspace
```

## License

Apache-2.0 — see [LICENSE](LICENSE) for details.
