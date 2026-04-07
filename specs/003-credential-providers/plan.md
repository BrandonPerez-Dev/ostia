# Plan: Credential Providers

> Date: 2026-04-07
> Status: complete (V0–V6 built)

## What & Why

Ostia sandboxes CLI tools but can't inject credentials into the sandbox. An agent
running `gcloud` commands needs an access token, but the sandbox is isolated from
the host's credential state. We need to fetch credentials on the host (via shell
commands, env vars, files, or vault HTTP APIs) and inject them as env vars into
the sandboxed process. This is critical for both local dev (shell out to
`gcloud auth print-access-token`) and production (vault API lookups keyed by user
identity).

Design follows the [External Secrets Operator](https://external-secrets.io/)
pattern: provider-agnostic interface where every provider returns flat key-value
pairs, and config maps those to injection targets.

## Constraints

- **`auth:` block replaced by `credentials:`** — no backward compat needed, nobody is using auth checks yet
- **Four provider types:** `command` (shell out, capture stdout), `env` (host env var), `file` (host file), `http` (vault/secrets API)
- **Uniform provider output:** all providers return `HashMap<String, String>` — `command`/`env`/`file` return `{ "value": content }`, `http` returns flattened top-level JSON keys
- **`inject` block maps provider output keys → sandbox env vars** — nothing injected without explicit mapping (whitelist)
- **Built-in presets for common tools** — `gcloud`, `github`, `aws` etc. as one-line config, overridable. Stored in `ostia-core` alongside built-in bundles
- **`execvp` → `execve`** — sandbox gets explicit env vector, no inherited parent env. Baseline vars (`PATH=/usr/bin:/bin`, `HOME=/`, `TERM=dumb`) set by sandbox
- **Crate boundaries:** `ostia-core` owns config schema + provider fetch logic + identity resolution. `ostia-sandbox` receives `HashMap<String, String>` env map. `ostia-cli` extracts identity from HTTP headers / CLI args
- **User identity for `http` provider:** resolution chain is `X-User-Id` header → `--user-id` flag / `OSTIA_USER_ID` env → implicit (none). Only `http` provider uses it (template variable in URL/headers)
- **Credential fetch happens on host before `fork()`** — same execution point as current auth checks
- **Failed credential fetch = blocked execution** — returns error to agent, never forks

## Non-Goals

- **Credential storage** — Ostia never persists secrets, only fetches and injects per-request
- **Credential rotation / caching** — fetch is per-execution (caching is a later optimization)
- **mTLS or certificate-based auth** — env var injection only
- **V10 tool description integration** — parallel work, will integrate later
- **Provider-specific vault SDKs** — `http` provider is generic (URL + JSON parsing), not HashiCorp/AWS-specific

## Build Skills (default for all verticals)

- rust-quality — Rust-specific patterns, error handling, testing conventions
- coding-standards — project conventions

## Verticals

### V0: Env injection into sandbox
- **Does:** Switch sandbox from `execvp` (inherits parent env) to `execve` with an explicit env map. Add `env: HashMap<String, String>` to `Profile`.
- **Done when:** A test profile with `env: { "TEST_VAR": "hello" }` produces a sandbox where `echo $TEST_VAR` outputs "hello", and parent env vars like `HOME` are NOT inherited.
- **Test:** Integration test — config with env map on profile, `run_command("echo $TEST_VAR")` returns "hello". Second assertion: `run_command("echo $HOME")` returns "/" (baseline), not the host user's home.
- **Deps:** None

### V1: Credential provider framework + `command` provider
- **Does:** Add `credentials:` config block to profiles. Implement provider enum with fetch logic. Wire `command` provider: shell out on host, capture stdout, map via `inject` block into profile env.
- **Done when:** A profile with `credentials: { gcp: { provider: command, command: "echo test-token", inject: { MY_TOKEN: "value" } } }` results in `echo $MY_TOKEN` returning "test-token" inside the sandbox.
- **Test:** Integration test — config with command provider using `echo` as mock, verify injected env var appears in sandbox output.
- **Deps:** V0

### V2: Built-in presets
- **Does:** Ship built-in credential presets for common tools (gcloud, github, aws). One-line config: `credentials: [gcloud]` expands to full provider + inject definition.
- **Done when:** `credentials: [gcloud]` in config resolves to command provider with `gcloud auth print-access-token` and injects `CLOUDSDK_AUTH_ACCESS_TOKEN`. Preset can be overridden by explicit config.
- **Test:** Unit test — preset resolution returns expected provider config. Integration test — preset name in config, mock command succeeds, env var injected.
- **Deps:** V1

### V3: `env` and `file` providers
- **Does:** Implement remaining simple providers. `env` reads a host env var. `file` reads a host file. Both output `{ "value": content }`.
- **Done when:** Config with `provider: env, env: "HOST_SECRET"` injects the value of the host's `HOST_SECRET` env var into the sandbox. Config with `provider: file, path: "/tmp/token.txt"` injects file contents.
- **Test:** Integration test — set host env var, config references it, verify sandbox sees injected value. Write temp file, config references it, verify sandbox sees contents.
- **Deps:** V1

### V4: `http` provider + user identity resolution
- **Does:** Implement `http` provider (GET URL, parse JSON response, map keys via `inject`). Implement user identity resolution chain (header → flag → implicit). Template variables in URL/headers: `{{ user_id }}`.
- **Done when:** Config with `provider: http, url: "http://localhost:PORT/secrets"` fetches JSON, maps response keys to sandbox env vars. User identity from `--user-id` flag or `X-User-Id` header is available as template variable.
- **Test:** Integration test — spin up mock HTTP server, config with http provider pointing to it, verify injected env vars match mock response. Test identity resolution: flag, env var, header (HTTP transport).
- **Deps:** V1
- **Skills:** ai-agent-building (MCP identity integration)

### V5: Remove old `auth:` system (headline — detail later)
### V6: Documentation + ESO pattern reference (headline — detail later)

## Open Questions

- **Baseline env vars:** Exact set for clean sandbox env — `PATH`, `HOME`, `TERM`, `USER`? What else do common CLI tools expect?
- **`http` provider auth:** How does the http provider itself authenticate to the vault? (API key in config? mTLS? Separate credential?) Keeping simple for now — static headers in config.
- **Preset list:** Which tools ship as built-in presets? Starting with gcloud, github (gh), aws. User can suggest more.
- **Template syntax:** `{{ user_id }}` vs `${user_id}` vs `{user_id}` — need to pick one. Leaning `{{ }}` (Jinja/mustache convention).
- **Concurrent credential fetch:** Should multiple credentials fetch in parallel? (Yes, eventually — but sequential is fine for V1.)
