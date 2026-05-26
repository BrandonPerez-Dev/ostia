# Capability: Profiles

## Intent

A profile defines a single sandboxed shell environment: which binaries are available, which subcommand patterns are allowed or denied, what filesystem paths are readable/writable, what credentials get injected, and (later) which domains are reachable. Profiles compose from reusable named bundles. The orchestrator selects a profile at init; the model cannot switch profiles mid-session.

## Config schema

```yaml
# ostia.yaml

auth:                            # optional — open (default) or token
  mode: open

bundles:                         # named binary sets with optional subcommand patterns
  baseline:
    description: "…"             # optional — if set, featured in tool description
    binaries: [cat, grep, ls, …]
    subcommands: []

  git-read:
    description: "git read-only"
    binaries: [git]
    subcommands:
      - "git log *"
      - "git diff *"
      - "git status"

profiles:
  dev:
    description: "…"             # optional — opening line of tool description
    bundles: [baseline, git-read, github-rw]   # resolved left-to-right
    tools:                       # additional binaries + subcommand allows
      binaries: [npm, node]
      subcommands: ["npm test *", "npm run *"]
    deny:                        # profile-level denies (override bundle allows)
      - "gh repo delete *"
      - "npm publish *"
    filesystem:
      workspace: /app/project    # read-write
      read: [/usr, /etc/ssl]     # read-only
      deny_read: [~/.ssh, ~/.aws, ~/.config, .env]
      deny_write: [.git/hooks, .bashrc, .zshrc]
    network:
      allow: [github.com, "*.github.com"]
    env:                         # literal env vars merged into sandbox env
      NODE_ENV: production
    credentials:                 # see spec/credentials.md
      gcloud: preset
      vault:
        provider: http
        url: "http://vault/secrets/{{ user_id }}"
        inject: { TOKEN: access_token }

endpoints:                       # HTTP-only — maps endpoint name → profile subset
  group: [alpha, beta]
```

## Bundle resolution rules

1. **Built-ins first, config overrides.** Ostia ships baseline, git-read, git-write, github-read, github-rw, k8s-read, docker, dev-tools as built-in bundles. A `config.yaml` that defines its own `baseline:` block overrides the built-in.
2. **Bundles merge additively** when composed by a profile. Binary sets union; subcommand allow-lists concatenate.
3. **Profile-level `deny:` overrides any bundle allow.** Deny is evaluated after the merged allow set.
4. **Unknown bundle name is a hard error** at profile-resolve time. Ostia refuses to start with an unresolvable bundle reference.
5. **Missing binary on the host is a soft warning.** The profile loads and commands that don't use the missing binary still run. `ostia check` shows `[missing]` vs `[found]`.

## Test contracts

The profiles capability has no direct integration tests — profile loading and bundle composition are exercised incidentally through the bundle tests under `sandbox.md` (B1–B7) and the MCP contracts under `mcp-server.md`. The contracts below are the ones that validate this capability's observable behavior.

### C-B1: Built-in baseline resolves without config definition
- **Test:** `builtin_baseline_resolves_without_config_definition` in `crates/ostia-cli/tests/builtins.rs`
- **Setup:** Config with a `test` profile declaring `bundles: [baseline]`, where `baseline` is not defined in the config file.
- **Action:** `ostia check --profile test`.
- **Expected:** Exit 0. Output lists `echo`, `cat`, `ls` from the built-in baseline.

### C-B2: Built-in git-read resolves without config definition
- **Test:** `builtin_git_read_resolves_without_config_definition` in `crates/ostia-cli/tests/builtins.rs`
- **Setup:** Config with a `test` profile declaring `bundles: [baseline, git-read]`, neither defined in config.
- **Action:** `ostia check --profile test`.
- **Expected:** Exit 0. Output includes `git`.

### C-B3: Config bundle overrides built-in
- **Test:** `config_bundle_overrides_builtin` in `crates/ostia-cli/tests/builtins.rs`
- **Setup:** Config defines `baseline` with only `[echo]`, overriding the built-in.
- **Action:** `ostia check --profile test`.
- **Expected:** Exit 0. Output contains `echo` and does NOT contain `cat` (the built-in entry is replaced, not merged).

### C-B4: Built-in bundle executes in sandbox
- **Test:** `builtin_bundle_executes_in_sandbox` in `crates/ostia-cli/tests/builtins.rs`
- **Setup:** `test` profile with `bundles: [baseline]` and workspace set.
- **Action:** `ostia run --profile test -- echo hello-from-builtin`.
- **Expected:** stdout `hello-from-builtin`; exit 0.

### C-B5: Unknown bundle produces error
- **Test:** `unknown_bundle_produces_error` in `crates/ostia-cli/tests/builtins.rs`
- **Setup:** `test` profile with `bundles: [nonexistent-bundle-xyz]`.
- **Action:** `ostia check --profile test`.
- **Expected:** Exit non-zero; stderr mentions "not found".

### C-B6: Missing binary check warns without crash
- **Test:** `missing_binary_check_warns_without_crash` in `crates/ostia-cli/tests/builtins.rs`
- **Setup:** Config bundle with `[echo, nonexistent-xyz-binary]`.
- **Action:** `ostia check --profile test`.
- **Expected:** Exit 0. Output shows `[missing]` for the nonexistent binary and `[found]` for `echo`. Graceful degradation — one missing binary does not fail profile load.

### C-B7: Missing binary allows other commands
- **Test:** `missing_binary_allows_other_commands` in `crates/ostia-cli/tests/builtins.rs`
- **Setup:** Bundle `[sh, bash, echo, nonexistent-xyz-binary]` with workspace set.
- **Action:** `ostia run --profile test -- echo degraded-ok`.
- **Expected:** stdout `degraded-ok`; exit 0. Command runs in the degraded profile.

## Invariants (untested but load-bearing)

- **Profile lock is session-wide.** Once `ostia serve` starts with a profile, no JSON-RPC call can switch to a different profile. Profile is derived from tool name in MCP mode, and locked via CLI flag in `ostia run` mode.
- **Mandatory deny paths cannot be overridden.** `~/.ssh`, `~/.aws`, `~/.config`, `.env`, `.git/hooks`, `.bashrc`, `.zshrc` are denied regardless of profile `read:` or `workspace:` configuration. (See sandbox.md C-S4 for the tested subset.)
- **`auth:` section was removed in V5 of credentials migration.** The credential provider framework (`credentials:`) replaces it. See `spec/credentials.md`.
- **Backwards-compatible source loading.** A `--config` YAML that was valid before Slice 1 of `changes/006-profile-source-providers/` landed must continue to parse and behave identically — implicit default is the `file` provider, bundles + profiles inline. See `spec/profile-source.md` for the broader contract.

## Non-goals

- Per-model profiles — profile is set by the orchestrator before the model sees anything.
- Flag-level parsing — subcommand gating uses pattern matching, not a CLI grammar parser.
- Dynamic profile creation from the model — profiles are declared in YAML at orchestrator init.
