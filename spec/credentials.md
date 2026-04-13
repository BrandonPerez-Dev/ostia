# Capability: Credentials

## Intent

Ostia sandboxes CLI tools but cannot let them share the host's credential state — an agent running `gcloud` commands needs an access token, but the sandbox is isolated from the host's auth configuration. This capability fetches credentials on the host (shell commands, env vars, files, or vault HTTP APIs) and injects them as environment variables into the sandboxed `execve`. Design follows the External Secrets Operator pattern: provider-agnostic interface where every provider returns flat key-value pairs, and an `inject` block maps those keys to sandbox env vars.

See `context/credential-pattern.md` for the design rationale.

## Provider types

| Provider | Fetch mechanism | Output shape |
|---|---|---|
| `command` | Shell out on host, capture stdout | `{ "value": <stdout> }` |
| `env` | Read a host env var | `{ "value": <env var value> }` |
| `file` | Read a host file | `{ "value": <file contents> }` |
| `http` | HTTP GET → parse JSON response | Top-level JSON keys, flattened |

All providers return `HashMap<String, String>`. The profile's `inject` block whitelists which keys flow into which sandbox env vars:

```yaml
credentials:
  gcp:
    provider: command
    command: "gcloud auth print-access-token"
    inject:
      CLOUDSDK_AUTH_ACCESS_TOKEN: value   # maps "value" → env var

  vault:
    provider: http
    url: "http://vault/secrets/{{ user_id }}"
    headers:
      Authorization: "Bearer static-vault-root"
    inject:
      ACCESS_TOKEN: access_token          # maps JSON .access_token → env var
      API_KEY: api_key                    # maps JSON .api_key → env var

  # Built-in preset — expands to full definition
  gcloud: preset
```

## Identity resolution

User identity is orthogonal to the profile. It's only consumed by the `http` provider, which interpolates `{{ user_id }}` into URLs and headers.

Resolution chain (first match wins):
1. `X-User-Id` HTTP header (for HTTP transport)
2. `--user-id <name>` CLI flag
3. `OSTIA_USER_ID` environment variable on the ostia process
4. No identity — `{{ user_id }}` is unresolved → credential fetch fails → execution blocked

## Execution model

- **Credential fetch happens on the host, before `fork()`.** The same execution point as the old auth check.
- **Failed fetch = blocked execution.** The agent sees an `isError: true` response before the sandbox is entered. Ostia never forks with missing credentials.
- **Sandbox gets `execve`, not `execvp`.** The env vector is constructed explicitly: baseline (`PATH`, `HOME`, `TERM`) + profile `env:` + injected credentials. No parent env leaks in.

## Test contracts — credential providers

### C-CR1: Config parsing rejects unknown provider
- **Test:** `credentials_config_rejects_unknown_provider` in `crates/ostia-cli/tests/credential_providers.rs`
- **Setup:** Config block `provider: ftp` (not a valid provider).
- **Action:** Spawn server, handshake, attempt `tools/list` and `tools/call`.
- **Expected:** Server rejects load at some point — either `tools/list` returns no tools, or the eventual tool call returns an error mentioning `ftp`.

### C-CR2: Command provider fetches and injects
- **Test:** `command_provider_injects_credential_into_sandbox` in `crates/ostia-cli/tests/credential_providers.rs`
- **Setup:** Profile with `credentials.gcp: { provider: command, command: "echo test-token", inject: { MY_TOKEN: value } }`.
- **Action:** `tools/call` with `command="echo $MY_TOKEN"`.
- **Expected:** stdout `test-token`.

### C-CR3: Failed fetch blocks execution
- **Test:** `failed_credential_fetch_blocks_execution` in `crates/ostia-cli/tests/credential_providers.rs`
- **Setup:** Credentials block where the command is `false` (exit 1).
- **Action:** `tools/call` with `command="echo hello"`.
- **Expected:** `isError=true`; message mentions "credential", the cred name, or "failed". The sandbox was never entered.

## Test contracts — env injection (execve baseline)

### C-EI1: Sandbox receives injected env vars
- **Test:** `sandbox_receives_injected_env_vars` in `crates/ostia-cli/tests/env_injection.rs`
- **Setup:** Profile with `env: { TEST_CRED: "injected-secret" }`.
- **Action:** `tools/call` with `command="echo $TEST_CRED"`.
- **Expected:** stdout `injected-secret`.

### C-EI2: Sandbox does not inherit parent env
- **Test:** `sandbox_does_not_inherit_parent_env` in `crates/ostia-cli/tests/env_injection.rs`
- **Setup:** Parent `ostia serve` process has `OSTIA_TEST_LEAK="should-not-see-this"` set. Profile defines no env.
- **Action:** `tools/call` with `command="echo $OSTIA_TEST_LEAK"`.
- **Expected:** stdout empty. Parent env is not inherited through `execve`.

### C-EI3: Baseline env vars are always present
- **Test:** `sandbox_has_baseline_env_vars` in `crates/ostia-cli/tests/env_injection.rs`
- **Setup:** Profile with no custom env.
- **Action:** `tools/call` with `command="echo $PATH:$HOME:$TERM"`.
- **Expected:** stdout `/usr/bin:/bin:/:dumb`. These three are unconditional.

## Test contracts — env and file providers

### C-CF1: `env` provider injects host env var
- **Test:** `env_provider_injects_host_env_var` in `crates/ostia-cli/tests/credential_env_file.rs`
- **Setup:** Host env has `HOST_SECRET="super-secret-value"`. Profile has `credentials.my-env-cred: { provider: env, env: "HOST_SECRET", inject: { INJECTED_SECRET: value } }`.
- **Action:** `tools/call` with `command="echo $INJECTED_SECRET"`.
- **Expected:** stdout `super-secret-value`.

### C-CF2: `env` provider missing var blocks execution
- **Test:** `env_provider_missing_var_blocks_execution` in `crates/ostia-cli/tests/credential_env_file.rs`
- **Setup:** Credentials block references `OSTIA_NONEXISTENT_TEST_VAR_12345` which is not set on the host.
- **Action:** `tools/call` with `command="echo hello"`.
- **Expected:** `isError=true`; message mentions "not set", the var name, or "missing".

### C-CF3: `file` provider injects host file contents
- **Test:** `file_provider_injects_host_file_contents` in `crates/ostia-cli/tests/credential_env_file.rs`
- **Setup:** Host file `<token_path>` contains `file-secret-123`. Profile has `credentials.my-file-cred: { provider: file, path: <token_path>, inject: { FILE_TOKEN: value } }`.
- **Action:** `tools/call` with `command="echo $FILE_TOKEN"`.
- **Expected:** stdout `file-secret-123`.

### C-CF4: `file` provider missing file blocks execution
- **Test:** `file_provider_missing_file_blocks_execution` in `crates/ostia-cli/tests/credential_env_file.rs`
- **Setup:** Profile references `/nonexistent/ostia-test-file-12345.txt`.
- **Action:** `tools/call` with `command="echo hello"`.
- **Expected:** `isError=true`; message mentions "not found", "No such file", or "nonexistent".

## Test contracts — HTTP provider + identity

### C-HT1: HTTP provider fetches JSON and injects multiple keys
- **Test:** `http_provider_fetches_json_and_injects` in `crates/ostia-cli/tests/credential_http.rs`
- **Setup:** Mock HTTP server on a local port responds with `{"access_token": "vault-token-abc", "api_key": "key-xyz"}`. Profile has `credentials.vault: { provider: http, url: "http://127.0.0.1:<port>/secrets", inject: { ACCESS_TOKEN: access_token, API_KEY: api_key } }`.
- **Action:** `tools/call` with `command="echo $ACCESS_TOKEN:$API_KEY"`.
- **Expected:** stdout `vault-token-abc:key-xyz`.

### C-HT2: HTTP provider server error blocks execution
- **Test:** `http_provider_server_error_blocks_execution` in `crates/ostia-cli/tests/credential_http.rs`
- **Setup:** Mock server returns HTTP 500.
- **Action:** `tools/call` with `command="echo hello"`.
- **Expected:** `isError=true`; message mentions "500", "server error", or "failed".

### C-HT3: HTTP provider interpolates user identity
- **Test:** `http_provider_interpolates_user_identity` in `crates/ostia-cli/tests/credential_http.rs`
- **Setup:** Mock server responds to `/secrets/user-42` with `{"token": "user-42-token"}`. Profile URL is `http://127.0.0.1:<port>/secrets/{{ user_id }}`. Server started with `--user-id user-42`.
- **Action:** `tools/call` with `command="echo $USER_TOKEN"`.
- **Expected:** stdout `user-42-token`. The mock server records that the request path was `/secrets/user-42`.

### C-HT4: Unresolved identity template blocks execution
- **Test:** `http_provider_unresolved_template_blocks_execution` in `crates/ostia-cli/tests/credential_http.rs`
- **Setup:** Profile URL contains `{{ user_id }}` but no `--user-id` was passed and no `X-User-Id` header is present.
- **Action:** `tools/call` with `command="echo hello"`.
- **Expected:** `isError=true`; message mentions `user_id`, `template`, or `identity`.

## Test contracts — built-in presets

### C-CP1: `gcloud` preset matches expected shape
- **Test:** `gcloud_preset_has_correct_command_and_inject` in `crates/ostia-core/tests/credential_presets.rs`
- **Setup:** Call `ostia_core::credentials::builtin_presets()`.
- **Action:** Access `presets["gcloud"]`.
- **Expected:** `provider == "command"`, `command == "gcloud auth print-access-token"`, `inject[CLOUDSDK_AUTH_ACCESS_TOKEN] == "value"`.

### C-CP2: Unknown preset name is a config error
- **Test:** `unknown_preset_produces_error` in `crates/ostia-core/tests/credential_presets.rs`
- **Setup:** Config has `credentials: { fakename: preset }` where `fakename` is not a built-in preset.
- **Action:** Load the config and call `resolve_profile("test")`.
- **Expected:** Err; message mentions `fakename`, "preset", or "unknown".

## Invariants (untested but load-bearing)

- **Credential fetch is sequential in V1.** Parallelism is a future optimization — the `execve` env vector is built from a sequential merge.
- **Provider output is a whitelist, not a passthrough.** Nothing reaches the sandbox without an explicit `inject` mapping — an HTTP response with an unexpected field is silently ignored unless the profile references it.
- **Ostia never persists secrets.** No file cache, no disk write. Every request re-fetches.
- **The `auth:` config section was removed in V5.** Old `auth:` blocks are no longer parsed; migration is the user's responsibility.

## Non-goals

- Credential rotation / TTL-aware caching — per-request fetch is the current policy.
- mTLS or certificate-based sandbox auth — env var injection only.
- Provider-specific SDKs (HashiCorp Vault, AWS Secrets Manager) — the `http` provider is generic.
- Credential scoping by which binary is being invoked — all credentials configured for a profile are injected on every tool call in that profile.
