# Credential Pattern

Ostia's credential layer follows the [External Secrets Operator](https://external-secrets.io/) pattern: a small, uniform interface over many secret sources, where the profile explicitly whitelists which keys flow where. This doc is the architectural "why" for the design decisions codified in `spec/credentials.md`.

## The problem

An agent running inside Ostia's sandbox has no access to the host's credential state. `gcloud auth print-access-token`, `gh auth token`, `aws sts get-caller-identity`, `vault kv get ...` all work on the host — but from inside the sandbox, the sandbox is isolated by design, so none of them can reach their config dirs, token caches, or the wider environment. Yet the whole point of sandboxing these tools is to let the agent use them for real work, which means real credentials.

## The design

**Fetch on the host, inject at `execve`.** Credentials are resolved on the orchestrator side before `fork()`, bundled into the env vector that `execve` will use, and forgotten. The sandbox sees environment variables; it never sees a credential-fetching mechanism.

```
┌─ Host (parent process) ──────────────────────┐
│                                              │
│  ostia-core::credentials::fetch()            │
│    ├─ command provider: shell out            │
│    ├─ env provider:     read host env        │
│    ├─ file provider:    read host file       │
│    └─ http provider:    GET URL → parse JSON │
│                                              │
│  Uniform output: HashMap<String, String>     │
│                                              │
└────────────────┬─────────────────────────────┘
                 │
                 ▼
    inject block (profile config)
    maps provider keys → sandbox env vars
                 │
                 ▼
┌─ Sandbox (child after fork + execve) ────────┐
│                                              │
│  Process sees only what the env vector       │
│  contains: baseline + profile env + injects  │
│                                              │
└──────────────────────────────────────────────┘
```

The sandbox has no knowledge of the provider layer. It receives a flat `HashMap<String, String>` as part of its env vector and that's it.

## Why ESO-inspired

The External Secrets Operator decouples three concerns that naive credential layers conflate:

1. **Where the secret lives** (HashiCorp Vault, AWS Secrets Manager, GCP Secret Manager, a shell command, an env var, a file, ...)
2. **How to fetch it** (API call, shell subprocess, read, JSON parse)
3. **Where it ends up** (which env var name in the target process)

ESO's answer is: every source has a provider that returns a flat key-value map, and a separate mapping block whitelists which keys go where. Ostia copies this shape one-to-one:

- `provider:` — which source type (`command`, `env`, `file`, `http`)
- The source's own config fields — `command:`, `env:`, `path:`, `url:` + `headers:`
- `inject:` — the mapping from provider output keys to sandbox env var names

The benefit: adding a new provider type (e.g., `vault` with a proper HashiCorp SDK) doesn't require changing anything above the provider layer. The profile config schema, the `inject` whitelist mechanism, the execve env vector — all stay the same. A new provider is a new variant in one Rust enum and a new implementation of one trait.

## Why uniform `HashMap<String, String>` output

All four providers return the same shape. `command`, `env`, and `file` return a single key (`{ "value": <content> }`) because they're inherently one-value sources. `http` returns the top-level JSON response keys flattened — so a response body of `{"access_token": "abc", "api_key": "xyz"}` becomes a map with two entries.

This uniformity means the `inject:` block has exactly one mental model: "map a key from the provider's output to an env var in the sandbox." No "oh, this is the command provider so the syntax is different" — the mapping syntax is identical regardless of source.

## Why `inject:` is a whitelist

The provider might return data the profile doesn't want exposed. An HTTP response could contain a field the vault operator added later, or a stale field from a previous version of the schema. If `inject:` were a passthrough ("map every provider key to an env var of the same name"), adding a new field upstream would silently leak into every sandbox.

Instead, `inject:` is an explicit mapping. Nothing reaches the sandbox env unless the profile names it. Upstream additions are invisible until a profile author chooses to opt in.

## Why fetch-before-fork

Two reasons:

1. **Failure handling is clean.** A credential that can't be fetched (vault down, command errors, env var unset, file missing) produces an `isError=true` response that the agent sees before the sandbox runs. There's no partial-execution state, no half-fetched env, no "the command ran but with wrong credentials."
2. **The sandbox is isolated from credential machinery.** The agent cannot fish for credentials by reading `/proc/self/environ` of ostia, or by looking at how the HTTP provider authenticates to the vault, or by inspecting the command provider's shell. Those all happen outside the sandbox boundary in the parent's address space.

Cost: there's no retry logic in the sandbox. If a vault fetch fails partway through a session, the agent sees `isError` and has to retry the call. In practice this is fine because the retry cost is another round of fork+exec, which is already cheap.

## Why `execve`, not `execvp`

`execvp` inherits the parent process's full environment. Parent env vars (including any credentials the operator loaded, any `OSTIA_*` config vars, the host user's `HOME`, `PATH`, etc.) would all leak into the sandbox unchanged.

`execve` takes an explicit env vector built by ostia. The vector is:

```
[
  "PATH=/usr/bin:/bin",
  "HOME=/",
  "TERM=dumb",
  ...profile env entries...,
  ...injected credentials...,
]
```

Nothing from the parent env is inherited. Contract `C-EI2` validates this: a parent process with `OSTIA_TEST_LEAK="should-not-see-this"` set spawns `ostia serve`, and inside the sandbox `echo $OSTIA_TEST_LEAK` returns empty. If `execvp` were still in use, that test would fail.

## Identity resolution

Some credentials depend on *who* the agent represents. A multi-tenant deployment might need `user-42`'s vault token, not `user-43`'s. The `http` provider supports templating:

```yaml
credentials:
  vault:
    provider: http
    url: "http://vault/secrets/{{ user_id }}"
    inject: { TOKEN: access_token }
```

The `{{ user_id }}` template is resolved from a chain (first match wins):

1. `X-User-Id` HTTP header on the JSON-RPC request (for HTTP transport)
2. `--user-id <name>` CLI flag on `ostia serve`
3. `OSTIA_USER_ID` env var on the ostia process
4. No identity → template is unresolved → credential fetch fails → `isError`

Identity is orthogonal to profile. A profile says "what tools are available"; identity says "who these credentials are for." Both are inputs to the per-request env vector build.

**Only the `http` provider uses identity.** The `command`, `env`, and `file` providers have no templating — they're source-agnostic key-value fetchers. Identity lives in the HTTP URL or headers, where it belongs for a vault-style integration.

## Built-in presets

Common tools get one-line config via presets:

```yaml
credentials:
  gcloud: preset      # expands to command provider
  github: preset
```

Presets are stored in `ostia-core` alongside built-in bundles. `gcloud` expands to:

```yaml
credentials:
  gcloud:
    provider: command
    command: "gcloud auth print-access-token"
    inject:
      CLOUDSDK_AUTH_ACCESS_TOKEN: value
```

Users can override a preset by writing the full block — the preset system is opt-in syntactic sugar, not a required abstraction.

## What credentials do NOT cover

- **Credential storage.** Ostia never persists secrets. No on-disk cache, no TTL-based memory cache. Every request re-fetches. A future optimization might add a short-lived in-process cache keyed by `(provider, user_id)`, but it would be opt-in and bounded by the duration of a single `tools/list` or `tools/call` invocation.
- **mTLS / certificate-based sandbox auth.** Env var injection only.
- **Provider-specific SDKs.** The `http` provider is generic — URL + optional headers + JSON parsing. Using HashiCorp Vault's full API (leases, renewals, namespaces) requires a proper SDK integration, which is future work.
- **Scoped credentials per binary.** If a profile declares credentials for two different tools, both sets are injected on every command in that profile. There's no "only inject GCP creds when gcloud is called." Scoping happens at the profile level, not at the call level.

## Migration from the old `auth:` system

V5 of the credential work removed the old `auth:` block entirely. The old system was executable check commands (`gh auth status`) that Ostia ran at init to decide whether to mark tools as "available" — it didn't fetch or inject anything. The new `credentials:` block fetches and injects. There's no backward compatibility shim; operators upgrading from the auth-check era have to rewrite their profiles.
