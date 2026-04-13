# Ostia — System Specification

Ostia is an OS-level sandbox for AI agents executing CLI commands. It gates binary access via Linux mount namespaces, enforces filesystem and syscall restrictions via Landlock and seccomp, and exposes sandboxed shell environments as MCP tools. Agents see only the binaries their profile whitelists; credentials are fetched on the host and injected as environment variables at `execve` time.

This directory holds the current behavioral contracts — the observable surface Ostia guarantees. Each capability file lists its integration test contracts (C-numbered) that are actually exercised in `crates/*/tests/`. Architectural "why" lives in `context/`; in-flight work lives in `changes/NNN-<topic>/`.

## Capability Index

| Capability | File | Scope |
|---|---|---|
| **Profiles** | [profiles.md](profiles.md) | YAML config schema, profile + bundle composition, built-in bundles, graceful degradation |
| **Sandbox** | [sandbox.md](sandbox.md) | Mount namespace binary allowlisting, subcommand matching, Landlock filesystem enforcement, seccomp syscall filtering, streaming execution |
| **Credentials** | [credentials.md](credentials.md) | Credential provider types (command/env/file/http), identity resolution, inject mapping, `execve` env model |
| **MCP Server** | [mcp-server.md](mcp-server.md) | stdio + HTTP transports, initialize handshake (+instructions), tools/list, tools/call, endpoint routing |
| **CLI** | [cli.md](cli.md) | `ostia serve`/`check`/`run` subcommands, output cleanliness, bind flags, env vars |

## What lives where

- **`spec/<capability>.md`** — the authoritative list of behavioral contracts. Edit in place when contracts change. Don't duplicate into `changes/`.
- **`context/<topic>.md`** — architectural truth: why the system is built this way. Crate layout, threat model, namespace strategy, credential design pattern, testing strategy. Hot memory for any future design session.
- **`changes/NNN-<topic>/`** — per-feature working area. Each folder contains `plan.md` and `test-plan.md` for an in-flight change. Contracts land *permanently* in `spec/`, not in `changes/`.
- **`specs/`** (legacy) — original `specs/NNN-<topic>/` folders from before the flat-capability layout. Kept as historical until migration is verified; nothing in `specs/` is load-bearing.

## Invariants that span capabilities

- **Profile is set at init time by the orchestrator, not the model.** The agent cannot switch profiles mid-session. Tool name = profile name; the URL (for HTTP) or the CLI flag (for stdio) is the authorization.
- **Two enforcement layers.** Mount namespace binary allowlisting is the hard OS boundary (bypass-proof). Subcommand pattern matching is defense-in-depth (parses the command string before exec). See `context/security-model.md`.
- **Fetch credentials on the host, inject at `execve`.** The sandbox uses `execve` with an explicit env vector — parent env vars never leak in. Failed credential fetch blocks execution before `fork()`. See `context/credential-pattern.md`.
- **All tests are integration tests against real sandboxes.** No mocked filesystems, no mocked namespaces. Tests spawn real `ostia serve` processes. See `context/testing-strategy.md`.
