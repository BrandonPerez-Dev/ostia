# Source Providers

Ostia's source-provider layer follows the same shape as its credential layer (`context/credential-pattern.md`): a small, uniform interface over many backends, where the operator picks one per deployment via config. This doc captures the architectural "why" that spans Slices 1-4 of `changes/006-profile-source-providers/`. The behavioral contracts live in `spec/profile-source.md` (Slice 1), `spec/binary-source.md` (Slice 3), and `spec/profile-registration.md` (Slice 4).

## The problem

Ostia's first generation packaged everything into the container image: bundles + profiles in YAML mounted as a volume, and CLI binaries (git, gh, jq, etc.) installed during the Docker build. Adding one new tool meant rebuilding and redeploying the image. Standing up a new vertical (trades, fleet, …) meant a new per-vertical deploy repo (`ostia-trades-deploy`) with its own image and its own YAML. This doesn't scale past one or two verticals, and it prevents Ostia from being the kind of generic sandbox runtime where the operator says "here's my datastore, give me a profile" and Ostia does the rest.

The shift: profile config and CLI binaries both become **data fetched from a pluggable source** at startup (and later, on demand). The container image stops being the source of truth for what tools exist and what profiles look like.

## The design

**Two sibling traits in `ostia-core::source`:**

```rust
trait ProfileSource {
    async fn load(&self, auth: &AuthSource) -> Result<SourcedConfig>;
}

trait BinarySource {
    async fn fetch(&self, manifest: &BinaryRef, auth: &AuthSource) -> Result<Vec<u8>>;
}
```

`SourcedConfig` is `{ bundles: HashMap<String, Bundle>, profiles: HashMap<String, ProfileDef> }` — the operational content. Bootstrap concerns (which provider, server-level auth mode, HTTP endpoints) stay in the local `--config` YAML and are parsed before any source is invoked.

`BinaryRef` is `{ name, source_provider_hint, ref: <sha256 | tag | path> }` — what to fetch and how. The cache layer is content-addressed so the same binary across many profiles dedupes naturally.

```
┌─ Host (parent process) ──────────────────────────────┐
│                                                      │
│  bootstrap config (--config YAML)                    │
│    profile_source: { provider: postgres, dsn, auth } │
│    binary_source:  { provider: http, base, auth }    │
│                                                      │
│           │                          │               │
│           ▼                          ▼               │
│   ProfileSource::load        BinarySource::fetch     │
│           │                          │               │
│           │                          ▼               │
│           │                  /var/lib/ostia/bin/...  │
│           │                  (warm cache, sha-keyed) │
│           │                          │               │
│           ▼                          ▼               │
│   OstiaConfig (full)         ResolvedBinary paths    │
│           │                          │               │
│           └──────────┬───────────────┘               │
│                      ▼                               │
│         resolve_profile(name) → Profile              │
│                      │                               │
│                      ▼                               │
│         SandboxExecutor::from_profile (unchanged)    │
└──────────────────────────────────────────────────────┘
```

The contract between this layer and `ostia-sandbox` is unchanged: still `Profile`, still the same fields. That's deliberate — the source layer is upstream of profile resolution, and profile resolution is upstream of sandbox construction. Each boundary has one job.

## Why mirror the credential-provider pattern

Three layers that ESO-inspired credential design already decoupled (see `context/credential-pattern.md`):

1. **Where the data lives** — file on disk, HTTP endpoint, Postgres table.
2. **How to fetch it** — read, GET, SELECT.
3. **How to authenticate** — env-resolved password, bearer token, mTLS client cert.

The source layer makes the same separation. The only twist is that `inject:` doesn't apply — source data isn't injected anywhere, it just *is* the config. So the trait is simpler: one method, returns parsed config or fetched bytes.

The benefit: adding a new provider (S3-style binary store, OCI registry, etcd) is a new trait impl in one place, not a sweep across the codebase. The bootstrap-config schema gains one new `provider:` value; the trait impl handles the rest.

## The bootstrap vs. source split

The bootstrap `--config` YAML is **operational** — what the deployer hands the container at deploy time:

```yaml
profile_source:   { provider: ..., ..., auth: {...} }
binary_source:    { provider: ..., ..., auth: {...} }   # Slice 3
endpoints:        { dev: [alpha, beta], ... }
auth:             { mode: open }                         # server-level
```

The source is **content** — what the deployer (or a future writer service) edits when they add new tools or profiles:

```yaml
bundles:
  baseline: { binaries: [...], subcommands: [...] }
  git-read: { ... }

profiles:
  alpha: { bundles: [baseline, git-read], filesystem: {...}, credentials: {...} }
  beta:  { ... }
```

Conflating the two would defeat "data, not infrastructure" — every profile add would require touching the deployment. The split also means the bootstrap config can be tiny in production (a half-page YAML pointing at Postgres) while the operational content grows freely.

For legacy backwards compat, an absent `profile_source:` block treats the bootstrap config as also containing the source data inline. Today's monolithic configs keep working unchanged. See `spec/profile-source.md` invariants.

## AuthSource enum — the shared auth surface

```rust
enum AuthSource {
    None,
    StaticSecret(SecretString),       // env-resolved password or bearer token
    DynamicToken { fetch_fn: ... },   // OAuth2 client creds, IAM token refresh — Slice 2+
    TlsIdentity { cert_path, key_path },
}
```

The four variants cover the realistic auth shapes (per research findings recorded in `changes/006-profile-source-providers/`). Each provider implementation translates the resolved credential into protocol-specific surface: Postgres DSN password field, HTTP `Authorization` header, TLS client cert on the handshake. The provider matches the variants exhaustively — adding a new provider means handling all of them or rejecting unsupported ones with a clear startup error.

**Secrets never live in YAML in clear.** Config fields are always indirection: `password_env: PG_PASSWORD`, `bearer_env: CONFIG_API_TOKEN`, `cert_path: /etc/ostia/client.pem`. The bootstrap YAML can be checked into git or mounted from a non-secret ConfigMap without leaking credentials.

`secrecy::SecretString` wraps in-memory credentials with zeroize-on-drop. The `Debug` impl prints `[REDACTED]` so logs don't leak the value when a provider fails.

## Why `tokio-postgres`, not `sqlx`

Ostia is a single static Rust binary. Binary size and zero-system-lib-dependency matter — operators install it via `cargo install` or pull a small Docker image. `sqlx` brings the compile-time query-checking macro infrastructure, the multi-driver abstraction, and embedded migrations machinery even when only one driver is used. `tokio-postgres` is Postgres-only with no macro layer, no migration runtime, and a clean async surface that fits Ostia's existing tokio/axum stack.

LISTEN/NOTIFY is first-class on `tokio-postgres` (`client.notifications()` → stream), which matters for Slice 2 refresh. `sqlx`'s `PgListener` is ergonomically nicer but the weight isn't worth it for a single LISTEN channel.

TLS is provided by `tokio-postgres-rustls` (pure-Rust rustls, no openssl link). For AWS RDS IAM auth, the `tokio-postgres-rustls-rds-demo` repo documents the CA cert path — deferred until a real user needs it.

Both features (`source-postgres`, `source-http`) are off by default in `ostia-core`'s feature flags. The minimal build (`file` provider only) keeps the binary small for users who don't need network sources.

## Fetch-vs-cache model

| Layer | What's cached | Lifetime | Invalidation |
|---|---|---|---|
| **Profile source** (Slice 1) | In-memory `OstiaConfig` after one source fetch at startup | Process lifetime | Restart |
| **Profile source refresh** (Slice 2) | Same, but with TTL polling or LISTEN/NOTIFY | Process lifetime | TTL expiry, push notification, or restart |
| **Binary source** (Slice 3) | Content-addressed tarballs in `/var/lib/ostia/binaries/<sha>/...` | Disk lifetime | Tarball deletion (manual or GC, future) |
| **Per-call sandbox executor** (existing) | Resolved binary paths + ELF deps in HashMap | One tool call | Discarded after the call |

The binary cache is the load-bearing piece for the perf budget. Network fetches happen at startup (eager pull based on declared profile binaries) or when a new profile is registered (Slice 4 background pull). Tool calls always read from the local cache; the cache contract is **warm-cache binary readiness < 10ms per tool call**, measured in `spec/binary-source.md`.

Content addressing means two profiles referencing `gh@v2.45.0` resolve to the same on-disk path. Storage stays bounded; integrity is verified at extraction time.

## What the source layer does NOT cover

- **Credential fetch and injection.** That's the credential provider layer (`spec/credentials.md`). The source layer fetches profile config; the credential layer fetches per-call secrets and injects them into the sandbox env. The two are orthogonal — a profile loaded from Postgres can still declare `credentials:` blocks that get resolved at tool-call time via the existing credential pattern.
- **Profile writes.** Ostia is read-only against the source. Adding/editing/deleting profiles happens via whatever external tool owns the data — a future agent-builder service, a SQL client, direct file edits, etc. The "who writes" question is intentionally out of scope.
- **Identity-templated source URLs.** `{{ user_id }}` is supported in credential URLs (per-request identity matters for vault-style fetches) but NOT in `profile_source.url` or `profile_source.dsn`. Profile selection is per-deployment, not per-request — identity threads through credentials, not through which profiles exist.
- **Per-call profile dispatch from the model.** Profile is still set by the orchestrator at init (CLI flag for stdio, endpoint URL for HTTP). The source layer changes *where* the profile data comes from, not *who picks* the profile.
- **Schema migrations.** Ostia does not run DDL. Operators create the Postgres tables themselves (or use whatever provisioning tooling their writer service ships with).
- **Connection pooling for Postgres.** Single client per `ostia serve` process. Profile loads are infrequent and Slice 1 has no per-request DB activity — pooling is premature.
- **OCI registry as a binary source.** Deferred. `http`, `s3-style`, and `file` cover Slice 3's immediate need. Adding OCI is a new provider in the same trait, no schema or contract change.
- **AWS-shaped auth (RDS IAM, SigV4).** Reserved as `AuthSource::DynamicToken` but not wired in Slice 1. `aws-config` is heavy; add when there's a real user.

## Why fail-closed on source error

The source is upstream of every profile. If it can't be loaded, no profile can be served, so there's no useful partial state. `ostia serve` exits non-zero before binding the listener — clients get connection-refused, not a stale-but-pretending-to-work server. This matches credential-fetch semantics: failure blocks execution, never silently degrades.

A future Slice 2 refresh failure has more options (keep serving the last good config, log loudly, retry with backoff) since there's already-loaded state to fall back on. Slice 1's startup case has no such fallback and must fail loud.

## Why one bootstrap config file, not a flag

`--config <path>` stays as the single configuration entry point. The alternative — `--profile-source postgres://...` style URL-on-flag — would split deployment knowledge across CLI args and an environment, and would force secrets into either flag values or a parallel env-var contract.

The bootstrap config file is the natural home for deployment shape. Operators already mount it into the container; adding `profile_source:` at the top of that same file is the smallest change possible. Container manifests, Helm charts, and systemd units don't need a new variable to set. The schema discrimination (legacy inline vs. explicit `profile_source:`) is a parser-level concern, not an interface change.
