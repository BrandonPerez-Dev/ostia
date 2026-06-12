---
status: planned
depends_on: [profile-source.md, sandbox.md]
---

# Binary Source

## Intent

Slice 3 of `changes/006-profile-source-providers/`. Slice 2 made profile config live; this slice makes the **CLI binaries** live too. Operators upload a binary to wherever (S3, an HTTP host, a Postgres BYTEA column, a local mount), describe its sha256 + format + source in the same datastore that holds profiles, reference it from a bundle, and the next ostia container picks it up — no image rebuild.

This is the second half of "data, not infrastructure" (`context/source-providers.md`). The first half (Slice 1+2) put profile config in a data layer. This slice puts the CLI bytes in the data layer too. The container ships with ostia itself and a handful of baseline binaries (sh, bash, echo) — everything operator-specific (gh, jq, kubectl, internal SDKs) comes from the source.

The `BinarySource` trait mirrors `ProfileSource`: same provider abstraction (`file`, `http`, `postgres-blob`), same `AuthSource` enum, same fetch-and-cache shape. The on-disk cache is content-addressed by sha256, so two profiles referencing the same `gh@v2.45.0` resolve to the same cache path — storage stays bounded; integrity verifies at extraction time.

Eager pull consumes Slice 2's `BinaryDiff` observable: when a successful profile-source refresh reports an added binary, ostia downloads it in the background so the next tool call is warm. Per-binary fail-open — one bad URL doesn't block other binaries; the missing binary surfaces as a clear error if a tool call eventually references it.

## Does

Load CLI binaries from a pluggable source at startup AND in response to live registry changes (via Slice 2's diff observable). Bind-mount cached binaries into the sandbox instead of relying on whatever happens to be on the container's host PATH.

## Done when

- A bootstrap `--config` YAML can declare `binary_cache_dir:` (default `/var/lib/ostia/binaries/`) and an optional `binary_source:` block.
- The source data layer (postgres / http / file) provides a top-level `binaries:` map describing each binary: sha256, format (`binary` or `tar.gz`), source provider, optional entry and libs.
- Bundles' `binaries:` list accepts heterogeneous entries: plain strings (resolve via registry → host PATH fallback) AND inline objects (`{name, source, sha256, format, entry?, libs?}`).
- Binaries land at `<binary_cache_dir>/<sha256>/<name>` (content-addressed). Tarballs land at `<binary_cache_dir>/<sha256>/` with the extracted tree underneath.
- Sandbox bind-mounts binaries from the cache path instead of (or in addition to) host PATH.
- On a successful `profile_source` refresh, ostia computes the binary diff (added names + shas vs. previous), and pulls added binaries into the cache before a tool call references them.
- A tool call to a profile whose binary isn't in the cache YET (cold-cache) blocks until the binary is staged, then proceeds. Worst case ~10s; failed pull → clear error to the agent.
- A failed pull does NOT block other binaries' pulls in the same diff event. The failure logs a loud stderr warning containing `binary source` and `pull`.
- Within one profile, two bundles referencing the same name with different shas is a startup error.
- Across profiles, different versions of the same name (`gh@<sha-A>` in profile X, `gh@<sha-B>` in profile Y) work side-by-side.
- A Slice 2 config that has no `binaries:` registry (legacy mode) still works: bundles' plain-string binaries fall back to host PATH exactly as today.

## Bootstrap config additions

```yaml
# --config /etc/ostia/ostia.yaml

profile_source:
  provider: postgres
  dsn: "postgres://ostia@db/ostia"
  cache_ttl: 30s
  auth: { type: static_secret, password_env: PG_PASSWORD }

# Slice 3 — new optional fields:

binary_cache_dir: /var/lib/ostia/binaries   # default; override for tests or non-standard layouts

binary_source:                              # optional. when absent, the binary registry comes from the same source as profile_source.
  provider: postgres                        # may differ from profile_source (split registry case)
  dsn: "postgres://ostia@binaries-db/ostia"
  auth: { type: static_secret, password_env: PG_BIN_PASSWORD }
```

When `binary_source:` is absent, ostia reads the binary registry from `profile_source` — the same source the profiles came from. Both shapes deliver identical behavior; the split exists only for operators who genuinely need different backends for the two registries (e.g., binary metadata in a small RDS, binary bytes in S3 referenced by URL).

## Source data additions

Source providers (`file`, `http`, `postgres`) gain a third top-level map alongside `bundles:` and `profiles:`.

**Schema for one binary entry** (`binaries.<name>`):

```yaml
binaries:
  gh:
    sha256: "9f8e7d6c..."             # required; verified after download
    format: binary                    # binary | tar.gz
    source:                           # nested BinarySource — same provider abstraction
      provider: http                  # file | http | postgres-blob
      url: "https://cdn.example/gh-v2.45.0"
      auth: { type: none }            # same AuthSource shape as profile-source
  jq:
    sha256: "1a2b3c..."
    format: tar.gz
    entry: bin/jq                     # required for tar.gz — path INSIDE the archive
    libs:                             # optional — additional paths to bind-mount from extracted tree
      - lib/libonig.so.5
    source:
      provider: postgres-blob         # bytes live in a BYTEA column ostia queries directly
      table: binaries_blobs
      key_column: name
      value_column: bytes
      key: jq-2.7.1
  myinternal-cli:
    sha256: "deadbeef..."
    format: binary
    source:
      provider: file                  # for air-gapped / local-dev cases
      path: /var/lib/ostia/uploads/myinternal-cli
```

**Postgres schema for the registry** (when `provider: postgres` is the source):

```sql
CREATE TABLE binaries (
    name        TEXT PRIMARY KEY,
    definition  JSONB NOT NULL,        -- { sha256, format, source: {...}, entry?, libs? }
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Optional companion table for the postgres-blob source provider
CREATE TABLE binaries_blobs (
    name        TEXT PRIMARY KEY,
    bytes       BYTEA NOT NULL,
    sha256      TEXT NOT NULL
);
```

Ostia at startup issues `SELECT name, definition FROM binaries` alongside the existing `bundles` and `profiles` queries.

## Bundle schema change

`bundles.<name>.binaries:` becomes a heterogeneous list. Two forms accepted; mix freely.

```yaml
bundles:
  baseline:
    # Today's plain-string form. Still works. Resolved against the registry
    # first; falls back to host PATH for built-ins like `echo`.
    binaries: [sh, bash, echo, cat, ls]

  github-cli:
    binaries: [gh]                     # resolves via registry

  pinned-jq:
    binaries:
      # Inline form — declares everything in-place, overrides any registry entry
      # for this bundle.
      - name: jq
        sha256: "abc123..."
        format: binary
        source: { provider: http, url: "https://cdn.example/jq-1.7-pinned" }

  mixed:
    binaries:
      - gh                              # registry lookup
      - { name: jq, sha256: "xyz...", format: binary, source: {...} }  # inline
      - echo                            # PATH fallback (no registry entry, built-in)
```

**Resolution order per bundle entry:**
1. Object form → use the inline ref directly.
2. String form, registered in the top-level `binaries:` map → use the registry entry.
3. String form, not registered → resolve via host PATH (`which`). This preserves today's behavior for built-in bundles (`baseline`, `git-read`, etc.) without forcing operators to upload `echo`.

## Provider types

| Provider | Fetch mechanism | Format support |
|---|---|---|
| `file` | Read local file at `path:` (or for tarballs, mmap+extract) | `binary`, `tar.gz` |
| `http` | HTTP GET to `url:` (rustls); `Authorization: Bearer <env>` if `auth.type=static_secret` with `bearer_env` | `binary`, `tar.gz` (discriminated by `format:` field, not Content-Type) |
| `postgres-blob` | `SELECT <value_column> FROM <table> WHERE <key_column> = $1` returns BYTEA | `binary`, `tar.gz` |

The `AuthSource` enum from Slice 1 is reused: `None`, `StaticSecret`, `TlsIdentity`, `DynamicToken` (reserved).

## Test contracts

### C-BS1: Top-level `binaries:` registry parses and a bundle resolves a registered binary
- **Test:** `binary_registry_parses_and_resolves_registered_name` in `crates/ostia-cli/tests/binary_source_registry.rs`
- **Setup:** Bootstrap config with `profile_source: { provider: file, path: <profiles.yaml> }`. The `profiles.yaml` declares `binaries: { gh: { sha256: ..., format: binary, source: { provider: file, path: <gh-fixture-path> } } }`, a `baseline` bundle with `binaries: [sh, bash, echo, gh]`, and a `test` profile referencing `baseline`. The `<gh-fixture-path>` is a tempfile created in test setup containing a tiny shell script that echoes a known string. The fixture's sha256 matches `binaries.gh.sha256`.
- **Action:** Spawn `ostia serve`. Complete handshake. Call `tools/call` with `name=test`, `command="gh"`.
- **Expected:** Tool call returns content containing the known string. The fixture's bytes were bind-mounted into the sandbox as `/usr/bin/gh` (or equivalent) from `<binary_cache_dir>/<sha>/gh`.
- **Side effect:** After the call, `<binary_cache_dir>/<sha>/gh` exists on disk and matches the fixture's bytes.

### C-BS2: Bundle inline ref takes precedence over registry entry of the same name
- **Test:** `inline_ref_overrides_registry_for_same_name` in `crates/ostia-cli/tests/binary_source_registry.rs`
- **Setup:** Source data declares `binaries: { gh: { sha256: <registry-sha>, ..., source: <pointed at fixture-A> } }`. A bundle has `binaries: [{ name: gh, sha256: <inline-sha>, format: binary, source: <pointed at fixture-B> }]`. Fixture A echoes "registry"; fixture B echoes "inline". Inline-sha ≠ registry-sha.
- **Action:** Spawn, handshake, `tools/call` with `command="gh"`.
- **Expected:** Output contains "inline" — the inline ref won. The cache holds `<inline-sha>/gh` (fixture B's bytes), not `<registry-sha>/gh`.

### C-BS3: Bundle plain name with no registry entry falls back to host PATH (built-in compat)
- **Test:** `plain_name_without_registry_falls_back_to_path` in `crates/ostia-cli/tests/binary_source_registry.rs`
- **Setup:** Source data has `binaries: { gh: {...} }` (only `gh` registered). A bundle has `binaries: [echo, gh]`. `echo` is NOT in the registry. Profile workspace points at a tempdir.
- **Action:** Spawn, handshake, `tools/call` with `command="echo path-fallback-works"`.
- **Expected:** Output contains `path-fallback-works`. `echo` was resolved via host PATH (today's `which`-based behavior). Bundle resolution did not fail just because `echo` isn't in the binary registry.

### C-BS4: Cross-profile multi-version — two profiles reference different shas of the same name, both work
- **Test:** `cross_profile_multi_version_both_work` in `crates/ostia-cli/tests/binary_source_registry.rs`
- **Setup:** Two fixtures: `gh-v1` echoes "version-1", `gh-v2` echoes "version-2", different bytes ⇒ different shas. Source data has no top-level `gh`. Profile `alpha` has a bundle with `binaries: [{ name: gh, sha256: <v1-sha>, source: <v1-path>, format: binary }]`; profile `beta` has a bundle with `binaries: [{ name: gh, sha256: <v2-sha>, source: <v2-path>, format: binary }]`.
- **Action:** Spawn, handshake. `tools/call alpha gh`. Then `tools/call beta gh`.
- **Expected:** Alpha returns "version-1"; beta returns "version-2". Both cache paths exist on disk simultaneously: `<binary_cache_dir>/<v1-sha>/gh` and `<binary_cache_dir>/<v2-sha>/gh`.

### C-BS5: Within-profile conflict (same name, different shas across bundles) is a startup error
- **Test:** `within_profile_conflicting_shas_blocks_startup` in `crates/ostia-cli/tests/binary_source_registry.rs`
- **Setup:** Profile `test` references bundles `A` and `B`. Bundle A has `binaries: [{ name: gh, sha256: <sha-A>, ... }]`. Bundle B has `binaries: [{ name: gh, sha256: <sha-B>, ... }]`. sha-A ≠ sha-B. (Source data, postgres or file — either works.)
- **Action:** Spawn `ostia serve`.
- **Expected:** Process exits non-zero before binding any listener. Stderr contains `gh`, the substrings `conflict` or `inconsistent` or `mismatch`, and the profile name `test`.

### C-BS6: HTTP binary source — single-binary format downloads and bind-mounts
- **Test:** `http_source_single_binary_lands_in_sandbox` in `crates/ostia-cli/tests/binary_source_http.rs`
- **Setup:** Local HTTP mock (multi-request helper from Slice 2) at `127.0.0.1:<port>` returns the bytes of a tiny shell-script binary on GET `/gh`. Registry declares `gh` with `source: { provider: http, url: "http://127.0.0.1:<port>/gh" }` and sha256 matching the served bytes.
- **Action:** Spawn, handshake, `tools/call` with `command="gh"` in a profile referencing `gh`.
- **Expected:** Output contains the script's echoed string. Cache file at `<binary_cache_dir>/<sha>/gh` exists, has matching sha. Mock recorded ≥1 GET.

### C-BS7: HTTP binary source — `.tar.gz` tarball extracts and bind-mounts the entry binary
- **Test:** `http_source_tarball_extracts_entry_binary` in `crates/ostia-cli/tests/binary_source_http.rs`
- **Setup:** Test generates a `.tar.gz` at runtime via `tar` + `flate2` dev-deps. The archive contains `bin/jq` (a fake binary — shell script) and `share/jq/license.txt` (filler). Mock server serves the tarball bytes. Registry: `jq: { sha256: ..., format: tar.gz, entry: "bin/jq", source: { provider: http, url: ... } }`.
- **Action:** Spawn, handshake, `tools/call jq`.
- **Expected:** Output contains the fake jq's echo. After the call, `<binary_cache_dir>/<sha>/bin/jq` exists on disk; the file is executable; sha matches.

### C-BS8: Postgres-blob binary source — BYTEA column → binary in sandbox
- **Test:** `postgres_blob_source_lands_in_sandbox` in `crates/ostia-cli/tests/binary_source_postgres.rs`
- **Setup:** testcontainers Postgres. Apply the `binaries`, `binaries_blobs`, `bundles`, `profiles` schema. Insert one row into `binaries_blobs` with `name=jq-2.7.1`, `bytes=<fake-jq-bytes>`, `sha256=<correct-sha>`. Insert one row into `binaries` with definition pointing at `provider: postgres-blob, table: binaries_blobs, key: jq-2.7.1`. Insert bundle + profile.
- **Action:** Spawn ostia serve pointing at the testcontainer DSN. Handshake. `tools/call jq`.
- **Expected:** Output contains the fake jq's echo. Cache file exists at `<binary_cache_dir>/<sha>/jq`.

### C-BS9: File binary source — local path → binary in sandbox
- **Test:** `file_source_lands_in_sandbox` in `crates/ostia-cli/tests/binary_source_file.rs`
- **Setup:** A tempfile contains the fake-binary bytes. Bootstrap config (file-provider source) registers `myinternal` with `source: { provider: file, path: <tempfile> }` and the matching sha256.
- **Action:** Spawn, handshake, `tools/call myinternal`.
- **Expected:** Output contains the binary's echoed string. Cache file at `<binary_cache_dir>/<sha>/myinternal` exists. (The cache COPY exists separately from the source path; ostia never bind-mounts directly from the source path because the source path may not be inside the sandbox's allowed filesystem.)

### C-BS10: Cold-cache tool call blocks until binary is pulled, then returns
- **Test:** `cold_cache_tool_call_blocks_until_pulled` in `crates/ostia-cli/tests/binary_source_lifecycle.rs`
- **Setup:** HTTP mock that returns the binary AFTER a small artificial delay (say 500ms). Registry references it. The cache dir is empty at spawn time (ostia did NOT pre-warm — e.g., the binary is new, just added since last refresh).
- **Action:** Spawn, handshake, `tools/call` referencing the binary. Time the call.
- **Expected:** Call returns successfully with the binary's output. Wall-clock latency >500ms (the artificial download delay). Subsequent call to the SAME profile completes much faster (warm cache; see C-BS14).

### C-BS11: Eager pull on Slice 2 refresh — new binary is in cache BEFORE next tool call
- **Test:** `eager_pull_on_refresh_diff_stages_binary` in `crates/ostia-cli/tests/binary_source_lifecycle.rs`
- **Setup:** testcontainers postgres. Seed initial state: bundle `baseline` (sh/bash/echo only — no `gh`), profile `test`. Bootstrap `cache_ttl: 1s`. HTTP mock serves a fake `gh` binary at a known URL. The registry's initial state has NO `gh` entry. Spawn ostia. Handshake.
- **Action:** Independent test client INSERTs `gh` row into `binaries` table (sha + format + source pointing at mock). Updates `bundles.baseline.definition` to include `gh` in its `binaries:` list. Sleeps 1.5s (past TTL). Before issuing any tool call, the test checks `std::fs::metadata(<binary_cache_dir>/<sha>/gh)` — must be Ok.
- **Expected:** The cache file `<binary_cache_dir>/<sha>/gh` exists on disk after the sleep and BEFORE any tool call. Proves the eager pull happened on the refresh tick, not lazily on tool call.

### C-BS12: Per-binary fail-open — one bad URL doesn't block other binaries in the same diff
- **Test:** `single_bad_binary_does_not_block_others` in `crates/ostia-cli/tests/binary_source_lifecycle.rs`
- **Setup:** Registry declares two binaries: `good` (HTTP mock serves correctly) and `bad` (HTTP mock returns 500). Both referenced by the same profile. cache_ttl short.
- **Action:** Spawn ostia. Wait for refresh + eager pull window. Check cache state: `good` should be staged; `bad` should NOT be in cache. `tools/call good` succeeds; `tools/call bad` returns a clear error.
- **Expected:** `<binary_cache_dir>/<good-sha>/good` exists. `<binary_cache_dir>/<bad-sha>/bad` does NOT exist. Stderr contains `binary source` and `pull` and the name `bad`. `tools/call good` returns the binary's output (no error). `tools/call bad` returns `isError=true` with content mentioning `bad` and either `not in cache`, `pull failed`, or `unavailable`.

### C-BS13: Tool call referencing an unfetched binary surfaces a clear error
- **Test:** `tool_call_to_uncached_binary_returns_clear_error` in `crates/ostia-cli/tests/binary_source_lifecycle.rs`
- **Setup:** Registry declares `unreachable` with `source: http` pointing at a closed port. Profile references it. Allow startup to succeed (registry parses; nothing tries to fetch until eager pull kicks).
- **Action:** Spawn. Allow eager pull to attempt (and fail). Call `tools/call unreachable`.
- **Expected:** Response is `isError=true`. Content mentions `unreachable` and either `not available`, `pull failed`, or `binary`. Does NOT panic, does NOT hang the connection.

### C-BS14: Warm-cache `tools/call` does not refetch the binary from the source
- **Test:** `warm_cache_tools_call_does_not_refetch` in `crates/ostia-cli/tests/binary_source_lifecycle.rs`
- **Setup:** Counted HTTP mock serving the binary. Spawn ostia, handshake. Initial `tools/call` to trigger the pull (or rely on eager pull).
- **Action:** After the first call completes, record `mock.request_count()`. Issue 5 more `tools/call` invocations in quick succession. Check `mock.request_count()` again.
- **Expected:** Request count after the 5 follow-up calls equals the count before. Zero additional GETs. The cached file was reused; the source was not re-queried. (Deterministic perf assertion — matches the C-PS16 pattern from Slice 2.)

### C-BS15: Sha256 mismatch on download is treated as a fetch failure
- **Test:** `sha256_mismatch_treated_as_fetch_failure` in `crates/ostia-cli/tests/binary_source_lifecycle.rs`
- **Setup:** Registry declares a binary with `sha256: <claimed>`. HTTP mock serves bytes whose actual sha is DIFFERENT.
- **Action:** Spawn. Allow eager pull. Check stderr; check cache state; call `tools/call`.
- **Expected:** Cache file does NOT exist (or, if temporarily written, was deleted after sha-check failed). Stderr contains the name AND either `sha256 mismatch` or `integrity`. `tools/call` to the binary returns `isError=true` with the same kind of error as C-BS13 (binary not available).

### C-BS16: Backwards compat — no `binaries:` registry, plain-name PATH fallback still works
- **Test:** `slice_2_config_without_binaries_registry_still_works` in `crates/ostia-cli/tests/binary_source_registry.rs`
- **Setup:** A Slice-2-shaped config: profile_source has bundles + profiles but NO `binaries:` map at top level. The bundle declares `binaries: [sh, bash, echo, cat]` — all built-ins, none in any registry (because there's no registry).
- **Action:** Spawn, handshake, `tools/list`, `tools/call` with `command="echo backwards-compat-ok"`.
- **Expected:** tools/list returns the profile. tools/call returns `backwards-compat-ok`. All binaries resolved via host PATH exactly as today. Cache dir need not exist or be populated.

## Tests

Registry + bundle resolution (`crates/ostia-cli/tests/binary_source_registry.rs`):
- `"binary_registry_parses_and_resolves_registered_name"` — covers § C-BS1.
- `"inline_ref_overrides_registry_for_same_name"` — covers § C-BS2.
- `"plain_name_without_registry_falls_back_to_path"` — covers § C-BS3.
- `"cross_profile_multi_version_both_work"` — covers § C-BS4.
- `"within_profile_conflicting_shas_blocks_startup"` — covers § C-BS5.
- `"slice_2_config_without_binaries_registry_still_works"` — covers § C-BS16.

HTTP provider (`crates/ostia-cli/tests/binary_source_http.rs`):
- `"http_source_single_binary_lands_in_sandbox"` — covers § C-BS6.
- `"http_source_tarball_extracts_entry_binary"` — covers § C-BS7.

Postgres provider (`crates/ostia-cli/tests/binary_source_postgres.rs`):
- `"postgres_blob_source_lands_in_sandbox"` — covers § C-BS8.

File provider (`crates/ostia-cli/tests/binary_source_file.rs`):
- `"file_source_lands_in_sandbox"` — covers § C-BS9.

Lifecycle (`crates/ostia-cli/tests/binary_source_lifecycle.rs`):
- `"cold_cache_tool_call_blocks_until_pulled"` — covers § C-BS10.
- `"eager_pull_on_refresh_diff_stages_binary"` — covers § C-BS11.
- `"single_bad_binary_does_not_block_others"` — covers § C-BS12.
- `"tool_call_to_uncached_binary_returns_clear_error"` — covers § C-BS13.
- `"warm_cache_tools_call_does_not_refetch"` — covers § C-BS14.
- `"sha256_mismatch_treated_as_fetch_failure"` — covers § C-BS15.

## Invariants

- **Content-addressed cache.** Binaries are stored at `<binary_cache_dir>/<sha256>/<name>` (single-binary) or `<binary_cache_dir>/<sha256>/<extracted-tree>` (tarball). Two source entries with the same sha256 share a cache path regardless of name; two with different shas never collide.
- **Sha verification before use.** Every downloaded blob (HTTP body, BYTEA bytes, file contents) is sha256-hashed before being placed in the cache. Mismatch → discard, log, treat as a fetch failure.
- **Ostia never executes a cached binary directly.** It only bind-mounts cached files into the sandbox namespace. The cache dir on the host is treated as untrusted blob storage.
- **Per-binary fail-open after the initial registry load.** The registry itself loads at startup with fail-closed semantics (unparseable JSONB / malformed registry entry → ostia exits non-zero, same as Slice 1's initial profile-source load). Once the registry is loaded, individual binary fetch failures are fail-open with loud warnings.
- **Cache-mediated bind-mount path.** When a binary is in the registry, the sandbox bind-mounts from the cache, NOT from host PATH — even if `which` would find a host PATH version. Operators upload binaries to control what runs inside the sandbox.
- **Warm-cache calls hit no source.** A `tools/call` whose binaries are already in the cache makes zero requests against `BinarySource` for those binaries. Enforced by C-BS14.
- **Cold-cache calls block, never silently drop.** A tool call referencing a missing binary either successfully blocks until the pull finishes, or returns an error after the pull fails. There is no "tool call succeeded with the binary missing" outcome.
- **Within-profile name uniqueness.** All bundles in a profile that reference name `gh` must agree on its sha256. A mismatch is a startup error. Across profiles, no such constraint.
- **Disk cache persists across restarts.** The cache dir is a persistent volume (in production). Restarting ostia does NOT re-pull cached binaries; the sha check verifies on first use after restart.

## Non-goals

- **`.tar.zst` format.** Deferred. `.tar.gz` covers Slice 3.
- **OCI registry as a binary source.** Deferred. http/file/postgres-blob cover the immediate need.
- **Version-templated URLs.** `url: "https://cdn.example/gh-{{version}}"` is not supported. Operators register `gh` per concrete version with a known sha.
- **Cache GC / eviction.** The cache grows monotonically in Slice 3. Cleanup is a future operator concern. Disk pressure is on the operator until a GC slice ships.
- **AWS IAM auth for binary sources.** Same posture as Slice 1: `AuthSource::DynamicToken` is reserved for a future slice that wires `aws-config`.
- **Per-call binary refresh.** Even with a stale cache and a fresh registry, ostia does NOT re-pull binaries lazily on every tool call. Refresh is gated on the Slice 2 `BinaryDiff` observable; the cache itself never expires.
- **Strict `<10ms` perf benchmark.** The contract test (C-BS14) asserts "warm cache is free," not a microsecond budget. Strict numeric perf belongs in an instrumented benchmark suite that's not part of the integration test gate.
- **Trusted-publisher signature verification.** Slice 3 verifies sha256 integrity against the registry's claimed value but does NOT verify that the registry-claimed sha was produced by an authorized publisher. Sigstore / Cosign / Sigsum integration is a future security slice.

## Notes

- **String-matched whitelisting vs binary-anchored enforcement** — flagged 2026-06-03. Brandon raised: with Slice 3, profiles will reference content-addressed binaries explicitly. That makes binary-level allowlisting much sharper than today's `which`-based discovery. Open question: should the subcommand string-matcher (`spec/sandbox.md` Layer 2 in `context/security-model.md`) be deprecated in favor of finer-grained binary registration (e.g., `gh-read` and `gh-write` as separate registry entries with different sha-pinned wrappers)? See `project_ostia_open_design_questions.md`. NOT a contract for this slice — surfaces as a design conversation if/when Slice 3 ships and the new shape proves comfortable.
- **Tarball entry resolution and goblin walk.** When a tarball is extracted, the `entry:` field points at the binary inside the tree. Ostia bind-mounts the entry binary AND walks its ELF deps via the existing `goblin` resolver. Deps resolve from (a) the tarball's bundled `libs:` first, (b) the cache dir's other extracted trees if shared, (c) the host's libs as a last resort. Bundled libs in `libs:` are bind-mounted alongside the entry.
- **PATH preservation for `which` fallback.** Today's built-in bundles (`baseline`, `git-read`, etc.) compile in name lists that resolve via `which`. Slice 3 preserves this exactly — the registry is OPT-IN for binaries the operator wants to manage; built-ins keep their PATH-based behavior.
- **Cache dir creation.** Ostia creates `<binary_cache_dir>/` (mkdir -p) at startup if missing. Failure to create (permission denied, read-only fs) is a startup error.
- **Tarball extraction safety.** Extraction must reject paths with `..`, absolute paths, or symlinks pointing outside the extraction root. A malicious tarball entry like `../../etc/passwd` would otherwise overwrite host files. Path-sanitization happens before any write.

## Changes
- 006 (2026-06-11) — initial creation. Slice 3 of `changes/006-profile-source-providers/`.
- 006 (2026-06-12) — test-writer landed red integration tests for C-BS1–C-BS16 across five new test files in `crates/ostia-cli/tests/binary_source_*.rs`. mcp_common extended with `make_fake_binary_bytes`, `sha256_hex`, `make_fake_tarball`, `start_counted_binary_mock`, `start_failing_binary_mock`, `start_delayed_binary_mock`, `temp_binary_cache_dir`. Dev-deps added: `sha2`, `tar`, `flate2`. Test state: C-BS3 + C-BS16 green for forward-compatible reasons (Slice 2's PATH-fallback path already handles "no registry, plain string"); C-BS1, C-BS2, C-BS4, C-BS5–C-BS15 red for the right reasons (registry not parsed, heterogeneous bundle shape rejected by current `Vec<String>` schema, no cache, no eager pull, no sha verification). Pre-existing Slice 1+2 suite verified non-regressing.
