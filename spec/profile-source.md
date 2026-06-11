---
status: built
depends_on: [profiles.md, mcp-server.md]
---

# Profile Source

## Intent

Ostia today reads profile config from a YAML file on disk. This makes a deployment static: adding a profile, a bundle, or a new tool means rewriting the file and (often) rebuilding the container image. This capability makes the profile config a **data fetch** at startup time — operators declare a `profile_source` in a small bootstrap `--config` YAML, and Ostia pulls the actual bundles + profiles from a `file`, an `http` endpoint, or a `postgres` database.

The shape mirrors the existing credential-provider pattern (`spec/credentials.md`, `context/credential-pattern.md`): a small `ProfileSource` trait, multiple implementations, the operator picks via config. Auth into the source uses a shared `AuthSource` enum so adding a new provider doesn't reinvent the auth surface.

This is Slice 1 of the broader change tracked in `changes/006-profile-source-providers/`. It only covers initial load-at-startup. Refresh / hot-reload is Slice 2; binary-tarball sources are Slice 3; hot profile registration is Slice 4. See `context/source-providers.md` (forthcoming) for the architectural rationale spanning these slices.

## Does

Load `bundles` and `profiles` from a pluggable source at startup. `file` is the implicit default for backwards compatibility; `http` and `postgres` are alternative providers selected via a `profile_source:` block in the bootstrap config.

## Done when

- A bootstrap `--config` YAML without a `profile_source:` block parses today's bundles + profiles inline (zero-change behavior).
- A bootstrap `--config` YAML with `profile_source: { provider: file, path: <other.yaml> }` loads bundles + profiles from the OTHER file.
- A bootstrap `--config` YAML with `profile_source: { provider: http, url: ... }` loads bundles + profiles from an HTTP GET response (YAML or JSON body).
- A bootstrap `--config` YAML with `profile_source: { provider: postgres, dsn: ..., auth: { password_env: ... } }` loads bundles + profiles from `bundles` and `profiles` tables in the database.
- HTTP and Postgres providers fail loudly on startup if the source is unreachable, returns an error, or the response/rows can't be parsed.
- An unknown `provider:` value is a config error before the server starts listening.
- All credential, sandbox, and MCP behavior downstream of profile resolution is byte-for-byte identical regardless of which source loaded the profile.

## Bootstrap config shape

```yaml
# --config /etc/ostia/ostia.yaml

# Legacy / dev mode (no profile_source block):
# bundles + profiles inline at top level (today's behavior, unchanged).
bundles: { ... }
profiles: { ... }
endpoints: { ... }
auth: { ... }

# OR: production mode with explicit source.
profile_source:
  provider: postgres | http | file
  # provider-specific fields below
  dsn: postgres://ostia@db/profiles      # postgres
  url: https://config.example/profiles   # http
  path: /etc/ostia/profiles.yaml         # file
  auth:
    type: none | static_secret | tls_identity   # dynamic_token deferred to later slices
    # type-specific fields:
    password_env: PG_PASSWORD             # for static_secret in postgres
    bearer_env: CONFIG_API_TOKEN          # for static_secret in http
    cert_path: /etc/ostia/client.pem      # for tls_identity
    key_path: /etc/ostia/client.key       # for tls_identity
  cache_ttl: 30s                          # Slice 2 — optional, default 30s.
                                          # Humantime-formatted duration (`30s`, `1m`,
                                          # `5m`). After this window expires, the next
                                          # `tools/list` or `tools/call` triggers a
                                          # source refresh.

endpoints: { ... }                        # always local — deployment shape
auth: { ... }                             # always local — server-level mode
```

When `profile_source:` is present, the top-level `bundles:` and `profiles:` keys in `--config` are ignored (with a startup warning logged if either is non-empty). `endpoints:` and `auth:` (server-level) stay local because they describe the deployment, not profile data.

## Provider types

| Provider | Fetch mechanism | Auth shapes (Slice 1) |
|---|---|---|
| `file` | Read a YAML file at `path:` | `none` only |
| `http` | HTTP GET to `url:`, body is YAML (`Content-Type: application/yaml` or absent) or JSON (`application/json`) | `none`, `static_secret` (bearer), `tls_identity` |
| `postgres` | Connect via `dsn:`, run `SELECT name, definition FROM bundles` and `SELECT name, definition FROM profiles` | `static_secret` (env-resolved password), `tls_identity` (mTLS client cert) |

`AuthSource` is an enum shared across providers: `None | StaticSecret(SecretString) | DynamicToken { fetch_fn } | TlsIdentity { cert, key }`. Slice 1 only implements `None`, `StaticSecret`, and `TlsIdentity`. `DynamicToken` is reserved for Slice 2+ (OAuth2 client credentials, AWS IAM tokens).

## Postgres schema (Slice 1)

```sql
CREATE TABLE bundles (
    name        TEXT PRIMARY KEY,
    definition  JSONB NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE profiles (
    name        TEXT PRIMARY KEY,
    definition  JSONB NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

`bundles.definition` is the JSONB equivalent of one entry in the YAML `bundles:` map: `{ description?, binaries: [...], subcommands: [...] }`.

`profiles.definition` is the JSONB equivalent of one entry in the YAML `profiles:` map: `{ description?, bundles: [...], tools?, deny: [...], filesystem?, network?, env?, credentials? }`.

`updated_at` is reserved for Slice 2 refresh; Slice 1 does not read it.

## Test contracts

### C-PS1: Legacy YAML config (no `profile_source:`) parses and runs identically
- **Test:** `legacy_yaml_config_no_profile_source_block_runs_unchanged` in `crates/ostia-cli/tests/profile_source_file.rs`
- **Setup:** `--config` is a today-style YAML using `write_mcp_config` (or equivalent) — `bundles` and `profiles` at top level, no `profile_source:` block.
- **Action:** Spawn `ostia serve`, complete handshake, `tools/list`, `tools/call` with `echo hello`.
- **Expected:** handshake succeeds; `tools/list` returns the inline profile; `tools/call` returns `hello` in content; `isError` absent/false.

### C-PS2: Explicit `file` provider loads from a different YAML file
- **Test:** `file_provider_explicit_path_loads_from_other_file` in `crates/ostia-cli/tests/profile_source_file.rs`
- **Setup:** Write `<tmpdir>/external.yaml` containing the real `bundles + profiles`. Write `<tmpdir>/bootstrap.yaml` containing only `profile_source: { provider: file, path: <tmpdir>/external.yaml }`. The bootstrap file has NO top-level `profiles:` block.
- **Action:** `ostia serve --config <tmpdir>/bootstrap.yaml`, handshake, `tools/list`, `tools/call` on the profile defined in `external.yaml`.
- **Expected:** `tools/list` contains the profile from `external.yaml`; `tools/call` executes successfully.

### C-PS3: Explicit `file` provider with `profiles:` also in bootstrap ignores the inline block
- **Test:** `file_provider_explicit_ignores_inline_profiles_in_bootstrap` in `crates/ostia-cli/tests/profile_source_file.rs`
- **Setup:** Bootstrap YAML has both `profile_source: { provider: file, path: <external> }` AND a top-level `profiles: { decoy: { ... } }` block with a profile named `decoy`. The external file defines a profile named `real`.
- **Action:** Spawn, handshake, `tools/list`.
- **Expected:** `tools/list` contains `real`; does NOT contain `decoy`. The source's data wins; the inline `profiles:` is ignored.

### C-PS4: `http` provider happy path with YAML body
- **Test:** `http_provider_loads_yaml_body` in `crates/ostia-cli/tests/profile_source_http.rs`
- **Setup:** Mock HTTP server on `127.0.0.1:<random-port>` responds to `GET /profiles` with `Content-Type: application/yaml` and a body containing valid `bundles + profiles` YAML. Bootstrap config: `profile_source: { provider: http, url: "http://127.0.0.1:<port>/profiles", auth: { type: none } }`.
- **Action:** Spawn, handshake, `tools/list`, `tools/call`.
- **Expected:** Profile from the HTTP body is visible in `tools/list`; `tools/call` returns expected output.

### C-PS5: `http` provider happy path with JSON body
- **Test:** `http_provider_loads_json_body` in `crates/ostia-cli/tests/profile_source_http.rs`
- **Setup:** Mock server returns `Content-Type: application/json` and the same logical content serialized as JSON.
- **Action:** Same as C-PS4.
- **Expected:** Same as C-PS4. Proves content-type discrimination.

### C-PS6: `http` provider sends bearer token from env var
- **Test:** `http_provider_bearer_token_from_env_var` in `crates/ostia-cli/tests/profile_source_http.rs`
- **Setup:** Mock server requires `Authorization: Bearer expected-token-xyz` and returns 401 otherwise. Set `OSTIA_TEST_CONFIG_API_TOKEN=expected-token-xyz` in the spawned `ostia serve` env. Bootstrap config: `profile_source: { provider: http, url: ..., auth: { type: static_secret, bearer_env: OSTIA_TEST_CONFIG_API_TOKEN } }`.
- **Action:** Spawn, handshake, `tools/list`.
- **Expected:** Mock server records that the incoming request had the correct `Authorization` header; handshake + `tools/list` succeed.

### C-PS7: `http` provider — 500 response is a startup failure
- **Test:** `http_provider_500_response_blocks_startup` in `crates/ostia-cli/tests/profile_source_http.rs`
- **Setup:** Mock server returns 500 for the config URL. Bootstrap config points at it.
- **Action:** Spawn `ostia serve`; attempt handshake.
- **Expected:** Process exits non-zero within a short window; stderr contains "profile source", "http", or "500"; the handshake never completes (client gets connection-closed or pipe-eof, not a JSON-RPC response).

### C-PS8: `http` provider — connection refused is a startup failure
- **Test:** `http_provider_connection_refused_blocks_startup` in `crates/ostia-cli/tests/profile_source_http.rs`
- **Setup:** Bootstrap config points at a port nothing is listening on.
- **Action:** Spawn; attempt handshake.
- **Expected:** Same shape as C-PS7 — process exits non-zero; stderr mentions the URL or "connection refused" or "profile source"; handshake never completes.

### C-PS9: `postgres` provider happy path
- **Test:** `postgres_provider_loads_bundles_and_profiles` in `crates/ostia-cli/tests/profile_source_postgres.rs`
- **Setup:** Start an ephemeral Postgres via `testcontainers-rs`. Apply the schema (`bundles`, `profiles` tables). Insert one bundle row (`baseline` → `{ binaries: [echo, cat], subcommands: [] }`) and one profile row (`test` → `{ bundles: [baseline], filesystem: { workspace: <ws> } }`). Bootstrap config: `profile_source: { provider: postgres, dsn: postgres://postgres@<host>:<port>/postgres, auth: { type: static_secret, password_env: OSTIA_TEST_PG_PASSWORD } }`. Spawned `ostia serve` env has `OSTIA_TEST_PG_PASSWORD=postgres`.
- **Action:** Spawn, handshake, `tools/list`, `tools/call` with `name=test`, `command="echo postgres-loaded"`.
- **Expected:** `tools/list` returns `test`; `tools/call` returns `postgres-loaded` in content.

### C-PS10: `postgres` provider — DSN must not embed password
- **Test:** `postgres_provider_dsn_password_resolves_from_env` in `crates/ostia-cli/tests/profile_source_postgres.rs`
- **Setup:** Same as C-PS9 but the `dsn:` in bootstrap config has NO `password=` query parameter. Password lives only in the env var named by `auth.password_env`.
- **Action:** Spawn, handshake.
- **Expected:** Connection succeeds. The DSN as written to disk doesn't leak the password.

### C-PS11: `postgres` provider — wrong password fails loudly
- **Test:** `postgres_provider_wrong_password_blocks_startup` in `crates/ostia-cli/tests/profile_source_postgres.rs`
- **Setup:** Same as C-PS9 but `OSTIA_TEST_PG_PASSWORD` is set to a wrong value.
- **Action:** Spawn; attempt handshake.
- **Expected:** Process exits non-zero; stderr mentions "authentication" or "password" or "profile source"; handshake never completes.

### C-PS12: `postgres` provider — connection refused fails loudly
- **Test:** `postgres_provider_connection_refused_blocks_startup` in `crates/ostia-cli/tests/profile_source_postgres.rs`
- **Setup:** Bootstrap DSN points at a port nothing is listening on (no testcontainer started).
- **Action:** Spawn; attempt handshake.
- **Expected:** Process exits non-zero; stderr mentions the host/port or "connection refused" or "profile source".

### C-PS13: Unknown `provider:` value is a config error
- **Test:** `unknown_provider_name_blocks_startup` in `crates/ostia-cli/tests/profile_source_file.rs`
- **Setup:** Bootstrap config: `profile_source: { provider: ftp, url: "..." }`.
- **Action:** Spawn `ostia serve`.
- **Expected:** Process exits non-zero before binding any listener; stderr contains `ftp`, "unknown", or "provider".

### C-PS14: HTTP bundles + profiles end-to-end through sandboxed execution
- **Test:** `http_source_executes_command_through_real_sandbox` in `crates/ostia-cli/tests/profile_source_http.rs`
- **Setup:** Mock HTTP server returns a profile that references a `baseline` bundle (also in the response) which includes `echo`. Profile has `filesystem.workspace` pointing at a tempdir.
- **Action:** `tools/call` with `command="echo via-http && date +%Y >> $WS/year.txt"` (where `$WS` is the workspace).
- **Expected:** stdout `via-http`; the file gets created in the workspace; `isError` absent/false. Proves the loaded profile reaches the real sandbox enforcement layer, not a stub.

### C-PS15: Postgres bundles + profiles end-to-end through sandboxed execution
- **Test:** `postgres_source_executes_command_through_real_sandbox` in `crates/ostia-cli/tests/profile_source_postgres.rs`
- **Setup:** Mirror of C-PS14 but bundles + profiles come from Postgres.
- **Action:** Same shape.
- **Expected:** Same shape. Proves Postgres-sourced data reaches the real sandbox.

## Test contracts — Slice 2 (live resolution + TTL cache)

### C-PS16: Cache hit within TTL prevents source refetch
- **Test:** `http_source_within_ttl_does_not_refetch` in `crates/ostia-cli/tests/profile_source_refresh.rs`
- **Setup:** Mock HTTP server backed by a request counter (new helper `start_counted_mock_server`) that records every incoming GET. Bootstrap config: `profile_source: { provider: http, url: ..., cache_ttl: 10s, auth: { type: none } }`. 10s is well above the wall-clock window the test will use.
- **Action:** Spawn `ostia serve`, complete handshake (counts as the initial load fetch). Call `tools/list` twice in quick succession (well within TTL).
- **Expected:** Both `tools/list` calls succeed with the same profile data. Mock server records exactly 1 GET total. The two `tools/list` calls were served from the in-process cache — no additional GETs.

### C-PS17: Cache expires after TTL → next call refetches and sees new data
- **Test:** `http_source_after_ttl_refetches_and_sees_new_data` in `crates/ostia-cli/tests/profile_source_refresh.rs`
- **Setup:** Stateful mock HTTP server (new helper `start_stateful_mock_server`) that initially returns profile YAML containing a single profile named `before`. The test can swap the body to one containing `after` mid-flight. Bootstrap: `cache_ttl: 1s`.
- **Action:** Spawn, handshake, `tools/list` → sees `before`. Swap the mock body to the `after` YAML. Sleep 1.2s (past TTL). `tools/list` → expected to see `after`.
- **Expected:** First list returns `[before]`; second list returns `[after]`. The cache expired and a refetch happened.

### C-PS18: Live profile addition (postgres) — new row visible after TTL
- **Test:** `postgres_source_picks_up_new_profile_after_ttl` in `crates/ostia-cli/tests/profile_source_refresh.rs`
- **Setup:** Testcontainers postgres seeded with one `baseline` bundle and one `before` profile. Bootstrap: `cache_ttl: 1s`. Spawn ostia, handshake, `tools/list` → `[before]`.
- **Action:** Open a separate tokio-postgres connection (the test's own, not ostia's) and `INSERT INTO profiles (name, definition) VALUES ('after', ...)`. Sleep 1.2s. `tools/list` again.
- **Expected:** Second list returns both `before` and `after`. Proves live additions are visible within TTL.

### C-PS19: Live profile removal (postgres) — deleted row gone after TTL
- **Test:** `postgres_source_picks_up_deleted_profile_after_ttl` in `crates/ostia-cli/tests/profile_source_refresh.rs`
- **Setup:** Testcontainers postgres seeded with `baseline` bundle and two profiles `alpha` and `beta`. Bootstrap: `cache_ttl: 1s`. Spawn, handshake, `tools/list` → `[alpha, beta]`.
- **Action:** Test deletes the `beta` row. Sleep 1.2s. `tools/list` again.
- **Expected:** Second list returns only `[alpha]`. Live removals are visible within TTL.

### C-PS21: Refresh failure is fail-open with loud warning
- **Test:** `http_source_refresh_failure_is_fail_open` in `crates/ostia-cli/tests/profile_source_refresh.rs`
- **Setup:** Stateful mock HTTP server that initially returns valid YAML, then can be flipped to return 500. Bootstrap: `cache_ttl: 1s`.
- **Action:** Spawn, handshake, `tools/list` → `[loaded]`. Flip the mock to 500. Sleep 1.2s. `tools/list` again.
- **Expected:** Second `tools/list` still returns `[loaded]` (last-good config; ostia did NOT exit, did NOT switch to empty data). Captured stderr (drained from the child process via a pipe-reader thread) contains both the substring `profile source` and the substring `refresh` — proving the loud warning ran.

## Test contracts — invariant cross-references (Slice 2)

- **Initial load remains fail-closed.** Already covered by C-PS7 / C-PS8 / C-PS11 / C-PS12 — those tests assert startup-failure when the source is unreachable at first load. Slice 2 does NOT relax this; the difference (fail-closed initial vs. fail-open refresh) is enforced by C-PS21 above.

## Tests

Slice 2 — live resolution + TTL cache (`crates/ostia-cli/tests/profile_source_refresh.rs`):
- `"http_source_within_ttl_does_not_refetch"` — covers § C-PS16.
- `"http_source_after_ttl_refetches_and_sees_new_data"` — covers § C-PS17.
- `"postgres_source_picks_up_new_profile_after_ttl"` — covers § C-PS18.
- `"postgres_source_picks_up_deleted_profile_after_ttl"` — covers § C-PS19.
- `"http_source_refresh_failure_is_fail_open"` — covers § C-PS21.

Note on C-PS16's red/green state at commit time: C-PS16 passes today because the pre-Slice-2 code loads at startup and serves from in-memory config forever (effectively infinite cache). Together with C-PS17 (red — TTL expiry must produce new data) they bound Slice 2's TTL behavior — infinite-cache passes 16 but fails 17; refetch-every-call would pass 17 but fail 16; only correct TTL respects both.

Slice 1 (built):

File provider (`crates/ostia-cli/tests/profile_source_file.rs`):
- `"legacy_yaml_config_no_profile_source_block_runs_unchanged"` — covers § C-PS1.
- `"file_provider_explicit_path_loads_from_other_file"` — covers § C-PS2.
- `"file_provider_explicit_ignores_inline_profiles_in_bootstrap"` — covers § C-PS3.
- `"unknown_provider_name_blocks_startup"` — covers § C-PS13.

HTTP provider (`crates/ostia-cli/tests/profile_source_http.rs`):
- `"http_provider_loads_yaml_body"` — covers § C-PS4.
- `"http_provider_loads_json_body"` — covers § C-PS5.
- `"http_provider_bearer_token_from_env_var"` — covers § C-PS6.
- `"http_provider_500_response_blocks_startup"` — covers § C-PS7.
- `"http_provider_connection_refused_blocks_startup"` — covers § C-PS8.
- `"http_source_executes_command_through_real_sandbox"` — covers § C-PS14.

Postgres provider (`crates/ostia-cli/tests/profile_source_postgres.rs`):
- `"postgres_provider_loads_bundles_and_profiles"` — covers § C-PS9.
- `"postgres_provider_dsn_password_resolves_from_env"` — covers § C-PS10.
- `"postgres_provider_wrong_password_blocks_startup"` — covers § C-PS11.
- `"postgres_provider_connection_refused_blocks_startup"` — covers § C-PS12.
- `"postgres_source_executes_command_through_real_sandbox"` — covers § C-PS15.

## Invariants

- **Backwards compatibility.** Any `--config` YAML that was valid before this slice landed must continue to parse and run with byte-identical behavior. Implicit default: no `profile_source:` block ⇒ inline bundles + profiles, file-on-disk. Tracked also in `spec/profiles.md`.
- **Initial load fails closed.** If the source is unreachable, returns an unparseable response, or rejects auth on the initial fetch at startup, `ostia serve` exits non-zero before accepting any client connection. A partially-loaded config is never visible to a client.
- **TTL-bounded in-process cache (Slice 2).** After a successful initial load, source data is cached in-process for `cache_ttl` (default 30s, configurable). The next `tools/list` or `tools/call` after the cache expires triggers a refresh. Supersedes the Slice 1 "single load at startup" invariant.
- **Refresh failure after initial load is fail-open.** Once an initial load has succeeded, subsequent refresh failures (source unreachable, returns non-2xx, etc.) do NOT shut down ostia. The last-good config keeps serving; a warning containing the substring `profile source` and `refresh` is written to stderr. Different from the initial-load-fail-closed posture above.
- **Per-process cache, not cross-container.** Each ostia process maintains its own in-memory cache. In a horizontally-scaled deployment (multiple ostia containers behind a load balancer), each container's view of the source can be stale by up to `cache_ttl` independently. Operators who need strict cross-container freshness should put a write-through cache between containers and the source — see non-goals.
- **Profile is set at init, not switched mid-session.** Same as today. The source can return many profiles; the orchestrator's choice of profile/endpoint at init is what the client sees.
- **No identity templating in source URLs.** `{{ user_id }}` is supported in credential URLs (`spec/credentials.md`) but NOT in `profile_source.url` or `profile_source.dsn`. Profile selection is per-deployment, not per-request.
- **Source data never includes secrets in clear.** Provider config fields named `*_env`, `cert_path`, `key_path`, etc. always point at external sources (env vars, files) — passwords, tokens, and keys are NEVER inline in the bootstrap YAML.

## Non-goals

- **Cross-container cache coherence.** Each ostia process has its own in-process cache; multi-container deployments will diverge by up to `cache_ttl` on each container until the next refresh. Operators wanting strict freshness across horizontal scale need a write-through cache layer between the containers and the source — that's end-architecture and out of scope for this slice.
- **Binary tarball fetching** — Slice 3 (`spec/binary-source.md` forthcoming). Slice 2 only proves the live-profile + TTL-cache abstraction; binaries still come from the container image as today.
- **Diff event consumers in Slice 2.** When a refresh detects added/removed binaries across profiles, the diff is computed and logged. Acting on the diff (background binary pull) is Slice 3's job. Slice 2 just makes the diff observable.
- **Hot profile registration as a separate slice.** Originally planned as Slice 4. After re-slicing on 2026-05-26, hot registration emerges as a natural consequence of Slice 2 (live cache) + Slice 3 (on-demand binary pull) — no separate spec needed.
- **Profile *writes*.** Ostia is read-only against the source. Writes happen via whatever external tool owns the data (e.g., a future agent-builder service).
- **AWS-shaped auth.** IAM tokens for RDS, SigV4 for S3-style endpoints, OIDC federation. Deferred until a real user needs them — `aws-config` is heavy.
- **OCI registry as a source.** `http` + `file` + `postgres` is the set across Slices 1–2. Adding OCI would be a new provider impl, same trait surface.
- **Per-call profile dispatch from the model.** Unchanged from today — profile is locked at init.
- **Connection pooling for Postgres.** Single client; reconnect on drop. Slice 1 volume doesn't justify pooling.
- **Schema migrations.** Operators create the `bundles` and `profiles` tables themselves; Ostia does not run DDL.

## Notes

- The split between bootstrap config (`--config` YAML — operational shape) and source data (bundles + profiles — content) is the load-bearing decision. The bootstrap is what the operator hands to the container at deploy time; the source is what they edit when adding new tools or profiles. Conflating the two would defeat the "data, not infrastructure" goal.
- HTTP body content-type discrimination (`application/yaml` vs `application/json`) keeps the on-the-wire format flexible without requiring a separate field on the bootstrap config. Default to YAML when ambiguous (matches the file provider's mental model).
- Postgres schema is intentionally minimal (no foreign keys between `profiles.bundles` references and the `bundles` table) — bundle resolution remains a runtime check inside `ostia-core`, identical to today's behavior when a profile references an unknown bundle.
- The `auth.type` discriminator on the bootstrap config maps 1:1 to `AuthSource` enum variants. `dynamic_token` is reserved on the type field but not implemented in Slice 1 — using it is a config error in this slice.

## Changes
- 006 (2026-05-25) — initial creation. Slice 1 of `changes/006-profile-source-providers/`.
- 006 (2026-05-26) — test-writer landed red integration tests for C-PS1–C-PS15 across three test files; spec `## Tests` section filled in with forward pointers. C-PS1 is green (backwards-compat contract); the other 14 are red until build implements the providers.
- 006 (2026-05-26) — build landed V0a (`ProfileSource` trait + `FileProfileSource` + bootstrap-config wiring in `serve.rs`) and V0b (`HttpProfileSource` via reqwest+rustls, `PostgresProfileSource` via tokio-postgres). All 15 contracts green; status flipped to `built`. Pre-existing test suite verified non-regressing.
- 006 (2026-06-11) — Slice 2 test-planning landed C-PS16, C-PS17, C-PS18, C-PS19, C-PS21 (cache hit, TTL expiry, live add/remove, refresh-fail-open). Added `cache_ttl:` to the bootstrap config schema (default 30s). Superseded "single load at startup" invariant with TTL-bounded cache invariant; added fail-open-on-refresh and per-process-cache invariants. Re-sliced the plan: original Slice 4 (hot registration) dissolved into Slices 2+3. C-PS20 (default TTL) intentionally skipped — default is documented in code/spec, no behavioral test. Status flipped from `built` → `in-progress` pending test-writer + build.
- 006 (2026-06-11) — test-writer committed Slice 2 tests in `crates/ostia-cli/tests/profile_source_refresh.rs`. New mcp_common helpers: `start_counted_mock_server`, `start_stateful_mock_server`, `MockResponseState`, `spawn_with_stderr_capture`. State at commit: C-PS17/C-PS18/C-PS19/C-PS21 red for the right reasons (no TTL cache yet, source loaded once at startup); C-PS16 already green for a forward-compatible reason (load-once-at-startup behavior also satisfies "within TTL no refetch") — together with C-PS17 it bounds Slice 2 behavior correctly. Pre-existing test suite verified non-regressing.
- 006 (2026-06-11) — build landed Slice 2: added `CachedProfileSource` wrapper (`crates/ostia-core/src/source/cached.rs`), `cache_ttl: Duration` field on each `*SourceDef` with humantime_serde (default 30s), `OstiaConfig::load_resolved_with_cache` returning the cache handle alongside the initial config, refactored `serve.rs::McpServer` to hold `Arc<RwLock<Arc<OstiaConfig>>>` + the cache, and added `refresh_if_due` that runs before each request handler (initialize / tools/list / tools/call). Fail-open warning to stderr matches the substring contract (`profile source` + `refresh`). Binary-diff observable emitted on successful refreshes for Slice 3 to consume. All 19 contracts green (C-PS1–C-PS19, C-PS21 — C-PS20 intentionally skipped). Pre-existing test suite verified non-regressing. Status flipped from `in-progress` → `built`.
