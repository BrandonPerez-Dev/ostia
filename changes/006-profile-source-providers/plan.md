# Plan: Profile + Binary Source Providers (Issue #11)

> Date: 2026-05-25
> Status: planning
> Issue: https://github.com/BrandonPerez-Dev/ostia/issues/11

## What & Why

Ostia today loads profiles from YAML on disk and CLI binaries from whatever the Docker image was built with. Adding a tool means rebuilding and redeploying the image; standing up a new vertical means a new deploy repo. This change makes both the profile config and the CLI binaries into **data fetched from a pluggable source** at runtime, so one Ostia container can serve many profiles and pick up new CLIs without an image rebuild.

The shape mirrors the existing credential-provider pattern (`command | env | file | http`): a small, uniform trait with multiple implementations the operator picks per deployment. `file` (today's behavior) is the implicit default; `http` and `postgres` are the production targets. Backwards compatibility for existing configs is a hard invariant.

## Spec changes

- `spec/profile-source.md` (new) — Slice 1. Provider trait for loading profile config. Implementations: `file`, `http`, `postgres`. Auth shape (`AuthSource` enum) lives here.
- `spec/profile-source.md` (extended) — Slice 2. Refresh semantics: TTL polling, `postgres` LISTEN/NOTIFY, `http` etag conditional GET.
- `spec/binary-source.md` (new) — Slice 3. Provider trait for fetching CLI binary tarballs into a local content-addressed cache. Eager pull at startup. Bind-mount path always points at the cache, never the network. Perf contract: warm-cache binary readiness < 10ms per tool call.
- `spec/profile-registration.md` (new) — Slice 4. Hot registration: a new profile added to the source triggers binary pull without restart.
- `spec/profiles.md` (modified) — Add backwards-compat invariant. Document the new optional top-level `profile_source:` key. The inline form remains valid and is the implicit default.
- `spec/cli.md` (modified) — No new flag in Slice 1. `--config` stays as the single entry point. Later slices may add observability/admin surfaces but not config-source flags.

## Context changes

- `context/source-providers.md` (new) — System-level rationale for the source-provider pattern. Sibling to `context/credential-pattern.md`, same shape: problem, design, why each provider, fetch-vs-cache model, identity vs auth, what's NOT covered. Grows as slices land.

## Constraints

### Source abstraction shape
- **Two sibling traits in `ostia-core`:** `ProfileSource` (returns the parsed `OstiaConfig` equivalent) and `BinarySource` (fetches tarball bytes + manifest). Both consume an `AuthSource`.
- **`AuthSource` enum, shared:** `None | StaticSecret(SecretString) | DynamicToken { fetch_fn } | TlsIdentity { cert, key }`. Each provider wires its resolved credential into the protocol-specific surface (postgres DSN, HTTP header, TLS handshake).
- **`secrecy` crate** for in-memory secret hygiene (zeroize-on-drop). Pulls into `ostia-core`.
- **Provider config syntax mirrors credentials:** `provider: postgres | http | file`, with provider-specific fields. Same `inject:`-style whitelist mechanic does NOT apply here — these are config sources, not secret sources.

### Backwards compat (invariant)
- **Any config valid before Slice 1 must parse and behave identically afterward.** Locked as an invariant on `spec/profiles.md`.
- **Implicit default:** absent `profile_source:` block ⇒ inline file provider. Today's `bundles:` + `profiles:` + `endpoints:` at top level keep working untouched.
- **Mode 2 (production):** explicit `profile_source:` block at top of `--config <path>` file. Inline `profiles:` becomes optional/ignored when the source is non-file. `endpoints:` stays at top level (it's a deployment concern, not profile data).

### Postgres provider
- **Crate: `tokio-postgres` + `tokio-postgres-rustls`.** Smaller binary than sqlx, no openssl link, native LISTEN/NOTIFY. Decision grounded in research findings (this folder).
- **Schema (suggested, finalized by test-planning):** a `profiles` table with `name TEXT PK`, `config JSONB`, `updated_at TIMESTAMPTZ`. Ostia issues `SELECT name, config FROM profiles` at load time. `bundles` come from the JSONB or a separate `bundles` table — test-planning to decide.
- **No connection pooling in Slice 1.** Single client; reconnect on drop. Profile loads are infrequent enough that pooling is premature.
- **Postgres password is fetched via `AuthSource::StaticSecret` whose value comes from an env var named in config** (e.g., `auth: { password_env: PG_PASSWORD }`). No passwords in YAML.

### HTTP provider
- **GET against URL, returns JSON or YAML body parsed into `OstiaConfig`.** Content-Type discriminates; default YAML to match the file provider's mental model.
- **Auth via `Authorization: Bearer $TOKEN`** when `AuthSource::StaticSecret` or `DynamicToken` is configured. mTLS via `TlsIdentity` for high-security deployments.
- **Identity templating (`{{ user_id }}`) is NOT supported in profile-source URLs.** Profile selection is per-deployment, not per-request. Identity is only meaningful for credentials.

### File provider
- **Wraps current YAML loader.** Walking skeleton (V0a) is: extract the existing `OstiaConfig::load(path)` body into a `FileProfileSource::load()`, plumb through the new trait, prove behavior unchanged via existing test suite.
- **Path can be absolute or relative to `--config <path>`** if file-as-subordinate-source is used.

### Perf budget (Slice 3, measured)
- **Warm-cache binary readiness < 10ms per tool call.** Measured by an integration test that times the path from "profile resolved" to "executor ready to fork." Today's `which` shellout + goblin walk is the baseline to beat after the cache shift.
- **Cold-cache (first call after registration) has no budget** — eager pull is supposed to prevent cold-cache ever happening in normal operation.

### Crate boundaries
- **`ostia-core`** gains: `source.rs` module with traits + `AuthSource` enum + `FileProfileSource`. Adds optional features `source-http` (reqwest with rustls) and `source-postgres` (tokio-postgres + tokio-postgres-rustls + secrecy). Both off by default to keep the minimal build small.
- **`ostia-sandbox`** unchanged. `Profile` contract preserved; bind-mount accepts any absolute path (verified: `crates/ostia-sandbox/src/namespace.rs:173-199`).
- **`ostia-cli`** gains: a tiny dispatch in `serve.rs` that picks the source based on `OstiaConfig.profile_source` and constructs the right provider before resolving any profile.

## Non-Goals

- **Who writes to the profile source.** Ostia is read-only. The write side (profile authoring UI, agent-builder-mcp endpoints, etc.) is out of scope.
- **Issue #12 credential broker formalization.** Adjacent but separate spec. Will likely re-use this design's `AuthSource` enum when it lands.
- **OCI registry as a binary source.** Defer to Slice 3+. `http` + `s3-style` + `file` cover the immediate need.
- **AWS-shaped auth (RDS IAM, SigV4) in Slice 1.** `aws-config` is a heavy dependency; add only when a real user needs it.
- **Per-call profile dispatch from the model.** Profile is still set by the orchestrator at init. This change moves *where* the profile data comes from, not *who picks* the profile.
- **Connection pooling for postgres** — single client per `ostia serve` process is enough until proven otherwise.
- **Profile-source identity templating.** No `{{ user_id }}` in source URLs. (Identity still applies to credentials downstream — unchanged.)

## Build skills

- **rust-quality** — Rust idioms, ownership/borrow patterns, idiomatic trait + enum design, no unwrap abuse on the new provider code.

## First slice

- **`spec/profile-source.md`** — the walking-skeleton entry point. Build's internal V0a/V0b split is:
  - **V0a (boundary scaffold):** Introduce `ProfileSource` trait + `AuthSource` enum in `ostia-core::source`. Wrap current YAML loading as `FileProfileSource`. Add optional top-level `profile_source:` parsing. All existing tests stay green. Zero behavior change for configs without the new block.
  - **V0b (walking skeleton wiring):** Add `HttpProfileSource` and `PostgresProfileSource` implementations + their integration tests (mock HTTP server, ephemeral Postgres or testcontainers). Wire dispatch into `serve.rs:run_serve`.

Once Slice 1's test contract is locked and the red tests are committed, build can start while Slices 2–4 get detailed.

## Open Questions

- **Slice 2 refresh policy default** — refresh off by default (require restart) vs. refresh-on-every-call vs. polled with a sensible TTL. Settle in Slice 2's test contract, not now.
- **Binary tarball manifest format (Slice 3)** — JSON manifest naming entry-point binary + interpreter + libs? Or rely on tarball internal layout? Out of scope for Slice 1 but will shape the binary-source contract.

## Test planning result

### Spec files created
- `spec/profile-source.md` (new) — 15 integration test contracts (C-PS1 through C-PS15). Covers backwards compat (file implicit default), file explicit form, http happy paths (YAML + JSON bodies), http auth (bearer from env), http failure modes (500, conn-refused), postgres happy path, postgres auth (env-resolved password), postgres failure modes (wrong password, conn-refused), unknown provider rejection, and two end-to-end sandboxed-execution checks for http + postgres.

### Spec files modified
- `spec/profiles.md` — added backwards-compatible source loading invariant.
- `spec/README.md` — added Profile Source row to capability index (marked planned).

### Mock boundaries
- **Real:** ostia process (spawned), HTTP mock server (in-process Rust HTTP server on 127.0.0.1, pattern from `credential_http.rs`), Postgres via `testcontainers-rs` (per-test ephemeral container).
- **Mocked:** none beyond the above. No SQLite-as-postgres, no in-memory HTTP fakes.

### Test infrastructure decisions (user-confirmed)
- **Postgres tests use `testcontainers-rs`** with skip-loud-on-missing-Docker (consistent with existing `docker.rs` pattern). Per-test ephemeral container. ~1-2s startup amortized across multiple tests in a file.
- **Bundles and profiles BOTH come from the source.** Bootstrap `--config` provides operational shape (source declaration, endpoints, server-level auth mode); the source provides all bundles + profiles data. Two-table Postgres schema (`bundles`, `profiles`, both keyed by name with JSONB definition).

### Settled open questions
- ~~Postgres schema details~~ → two-table schema locked in `spec/profile-source.md` (`bundles`, `profiles`, both `name TEXT PK + definition JSONB + updated_at`).
- ~~Testcontainers vs manual postgres~~ → testcontainers-rs.

### Context updates planned
- `context/source-providers.md` (new) — system-level rationale. Forthcoming separate task.
