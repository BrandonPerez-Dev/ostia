# Security Model

Ostia's security story is the product. Without it, this is a wrapper around `sh -c`. This doc explains what Ostia actually protects against, where the hard boundaries are, and what's defense-in-depth.

## Threat model

The attacker is an AI agent that has been prompt-injected, or has made a reasoning mistake, and is trying to execute a binary, read a file, or reach a host the orchestrator did not intend. The defender is the orchestrator, who set the profile at init time before handing control to the model.

Key assumption: **the orchestrator is trusted, the model is not.** The model can compose arbitrary commands, evaluate shell indirection, encode payloads in base64 — anything it can type is in scope. What it cannot do is change the profile, because profile selection happens before the model sees any tool.

## Two enforcement layers

Ostia has two layers of enforcement with very different trust properties.

### Layer 1 — OS-level (hard boundary, bypass-proof)

**Mount namespace + bind-mounted binaries.** Binaries not in the profile whitelist do not exist in the agent's filesystem view. Not "exist but you don't have permission" — genuinely absent. The error, if the agent tries, is `No such file or directory`, not `Permission denied`.

This layer cannot be bypassed by:
- Shell indirection (`eval`, `source`, `$(...)`, backticks)
- Encoding tricks (`echo <base64> | base64 -d | sh`)
- Symlink chasing (even if a symlink points to a real binary, that binary isn't in the namespace)
- Subshells (`bash -c '...'`, `sh -c '...'`)
- `exec`-family syscalls called directly by compiled binaries — the target file simply isn't on the rootfs

This works because all of it runs inside a fresh mount namespace constructed by `unshare(CLONE_NEWNS)` → `pivot_root`. The namespace is populated only with what the profile declares.

**Landlock filesystem rules** layer on top of the mount namespace. Workspace is read-write, `read:` paths are read-only, `deny_read:` and `deny_write:` override everything including the workspace. Mandatory deny paths (`~/.ssh`, `~/.aws`, `.env`, `.git/hooks`, `.bashrc`, `.zshrc`) are applied regardless of profile configuration and cannot be overridden.

**Seccomp BPF filter** blocks `mount`, `unshare`, `clone` (with namespace flags), `ptrace`, `kexec_load`, `open_by_handle_at`. This is what prevents a whitelisted `unshare` binary from being used to escape the sandbox — the syscall is denied even if the binary is available.

**`prctl(PR_SET_NO_NEW_PRIVS, 1)`** is applied before `execve`, so setuid binaries (if any somehow got into the namespace) cannot elevate privilege.

### Layer 2 — defense-in-depth (pattern matching)

**Subcommand pattern matching** validates the command string before the sandbox is entered. Compound commands (`&&`, `||`, `;`, `|`) are split and each subcommand is matched against the profile's glob patterns. Rejection happens on the orchestrator side — the agent never gets to `execve`.

This layer is softer. It validates intent ("can this agent run `gh pr merge`?") not capability ("is `gh` available?"). A sophisticated attacker with control of the command string could potentially confuse the pattern matcher with unusual whitespace, escapes, or shell constructs the matcher doesn't parse. That's why it's defense-in-depth — if Layer 2 fails, Layer 1 still holds.

The asymmetry is deliberate. Layer 1 ("what binaries exist") is a kernel-enforced invariant. Layer 2 ("what invocations are allowed") is a usability convenience — let the planning agent read GitHub but not merge. The hard security claim is Layer 1.

## What Ostia does NOT protect against

- **Holes in the binary itself.** If `gh` is in the profile and `gh` has a command injection vulnerability, Ostia doesn't catch it. Scope your allowed binaries to trustworthy tools.
- **Whitelisted binaries with shared writable state.** `gh` reading `~/.config/gh/` means that dotfile directory is reachable if mounted. Dotfile support is opt-in via profile config.
- **Kernel bugs.** Ostia trusts the kernel's namespace and Landlock implementation. A kernel vuln in user namespaces or mount plumbing is beyond scope.
- **Container-level breakouts.** If Ostia runs inside a Docker container and the container itself is compromised, the sandbox doesn't help. Use Ostia + container isolation as layered defenses, not Ostia as a container replacement.
- **Timing or side-channel attacks.** No mitigation for cache-timing, spectre, speculative execution leaks.
- **Denial of service.** A whitelisted binary can consume CPU/memory/disk within its allowed scope. Ostia doesn't cap resource usage in v1.

## Fail-closed defaults

- **Deny network by default.** No `network:` config means no network access for the sandbox.
- **Deny writes outside workspace by default.** No `workspace:` means no writable paths inside the sandbox.
- **Fail-closed on credential fetch.** A credential provider that fails to return output blocks the execution before `fork()`. The sandbox is never entered with missing credentials.
- **Mandatory deny paths** cannot be overridden by any profile. Paths like `.ssh`, `.aws`, `.env` are deny-read regardless of config.

## Why no user namespace

`CLONE_NEWUSER` would give us uid/gid mapping so the sandbox process appears as uid 0 inside the namespace without being root on the host. That's appealing, but it comes with problems:
- **Distro restrictions.** Ubuntu 24.04 added AppArmor restrictions on unprivileged user namespaces (see issue #2). Any design that depends on user namespaces is gated on those sysctls.
- **Config complexity.** uid/gid maps have to be written to `/proc/self/uid_map` and `/proc/self/gid_map` before most syscalls work, and the process has to become `nobody:nogroup` in between. This is fiddly and error-prone.
- **Mount namespaces work without them.** On modern kernels with `kernel.unprivileged_userns_clone=1` (which we already require), we can create mount namespaces without a user namespace. That's the path Ostia took.

Trade-off: the sandbox process runs as the same uid as the parent. Breakouts via setuid binaries would be worse if we didn't also set `PR_SET_NO_NEW_PRIVS`, which we do.

## Why no PID namespace

`CLONE_NEWPID` would isolate the process tree, but it requires the first process in the new namespace to act as PID 1, which means reaping zombie children and handling signals specifically. That's a non-trivial bit of code that would need its own test surface. Deferred past v1; the cost of not having it is that sandboxed processes can see the parent process tree via `/proc`, which we already scope via mount namespace (`/proc` is freshly mounted with `MS_NOSUID | MS_NOEXEC | MS_NODEV`).

## Why no network namespace (yet, tested)

`CLONE_NEWNET` is planned and the code path exists, but the host-side HTTP/SOCKS proxy needed to make it useful is substantial (500-800 lines of `hyper` + domain allowlisting). No integration tests cover network isolation yet. Until that ships, the default fail-closed is that outbound network goes through whatever the parent process had access to — which is why production deployments should layer Ostia inside a container with its own network restrictions.

## The model cannot change the profile

This is load-bearing. The profile is chosen by the orchestrator — typically a CLI flag or an endpoint URL — before any model input is accepted. There is no JSON-RPC method, no tool call, no special command that lets a running session switch to a different profile. The only way to change profiles is to restart `ostia serve` with a different argument.

In HTTP transport, this means the endpoint URL (`/mcp/dev`, `/mcp/readonly`, or `/mcp/{custom-endpoint}`) IS the authorization boundary. Per-request auth tokens (issue #4) will add another layer on top, but the current authorization story is: whoever has the URL has the profile.
