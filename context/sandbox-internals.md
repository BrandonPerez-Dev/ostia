# Sandbox Internals

The exact sequence and rationale for how `ostia-sandbox` constructs a namespace, populates it, applies restrictions, and execs the target command. If you're changing anything in `crates/ostia-sandbox/src/`, start here.

## Entry point

```rust
ostia_sandbox::SandboxExecutor::from_profile(profile: ostia_core::Profile)
```

Builds an executor holding the resolved binaries, subcommand patterns, filesystem paths, env, and credentials. Two execution methods:

- `execute(cmd)` — blocking, returns `ExecutionResult`
- `execute_streaming(cmd, tx)` — non-blocking, streams `Chunk::Stdout / Stderr / Exit` events through a channel
- `execute_streaming_collect(cmd)` — wraps the streaming API into an `ExecutionResult`

All three go through the same namespace-setup sequence. The difference is only in how output is captured.

## Execution sequence

Per command invocation (not per profile — the sandbox is torn down and rebuilt each call, which is the unit of isolation between tool calls):

```
1.  Validate the command string
    - Split on &&, ||, ;, | (shell-aware, respecting quotes)
    - For each subcommand, extract binary name
    - If binary not in profile's whitelist → reject (binary not whitelisted)
    - If subcommand doesn't match any allow pattern and an allow list exists → reject
    - If subcommand matches any deny pattern → reject
    - Rejection returns ExecutionResult { allowed: false, reason: "...", exit_code: 127 }

2.  fork()

    In the parent:
3.  Wait on child, read stdout/stderr pipes (or stream through channel)
4.  Collect ExecutionResult

    In the child:
5.  unshare(CLONE_NEWNS)
      // Mount namespace only — no CLONE_NEWUSER, no CLONE_NEWPID in v1
      // CLONE_NEWNET is a planned addition for network isolation

6.  mount(NULL, "/", NULL, MS_REC | MS_PRIVATE, NULL)
      // Kill mount propagation to the host

7.  Create NEWROOT at a tmpfs mount
      mount("tmpfs", NEWROOT, "tmpfs", MS_NOSUID | MS_NODEV, "size=64m")

8.  For each whitelisted binary:
      a. Resolve binary path via `which` crate
      b. Use goblin to parse ELF, extract DT_NEEDED, RPATH, RUNPATH
      c. Recursively resolve shared lib dependencies (ld.so.cache lookup)
      d. Bind-mount binary to NEWROOT/<same-path> (two-step: create empty file, bind-mount onto it, read-only)
      e. Bind-mount each resolved library (read-only)
      f. Bind-mount the ELF interpreter (/lib64/ld-linux-x86-64.so.2 or equivalent)

9.  Create library path symlinks
      /lib -> /usr/lib
      /lib64 -> /usr/lib64

10. Bind-mount /bin/sh (always — needed to run `sh -c "<command>"`)

11. Mount fresh /proc (MS_NOSUID | MS_NOEXEC | MS_NODEV)
      // Fresh /proc scoped to the new namespace

12. Bind-mount minimal /dev
      /dev/null, /dev/zero, /dev/urandom, /dev/random, /dev/full

13. Bind-mount workspace (read-write)
14. Bind-mount read paths (read-only)
15. Bind-mount credentials Unix socket (future — for network proxy / credential broker)

16. pivot_root(NEWROOT, NEWROOT/oldroot)
    chdir("/")
    umount2("/oldroot", MNT_DETACH)
    rmdir("/oldroot")
      // At this point, the old rootfs is unreachable from the child

17. prctl(PR_SET_NO_NEW_PRIVS, 1)
      // Must come before Landlock and seccomp — setuid binaries
      // can no longer elevate privilege

18. Apply Landlock ruleset
      - LANDLOCK_ACCESS_FS_WRITE_FILE / MAKE_REG denied outside workspace
      - mandatory deny paths (.ssh, .aws, .env, .git/hooks, ~/.bashrc, ~/.zshrc)
      - Compat mode: if Landlock ABI < expected, log warning and run
        with mount-namespace isolation only

19. Apply seccomp BPF filter
      - Blocked: mount, unshare, clone (with namespace flags),
        ptrace, kexec_load, open_by_handle_at
      - Everything else: allow
      - Filter is a deny-list, not an allow-list — minimizes false positives

20. Build env vector (execve-style)
      - PATH=/usr/bin:/bin
      - HOME=/
      - TERM=dumb
      - USER=(unset — sandbox has no user identity)
      - + profile env
      - + injected credentials (already fetched on host)

21. execve("/bin/sh", ["-c", "<command>"], env_vector)
```

## Why `execve` and not `execvp`

`execvp` inherits the parent process's full environment. That would leak every env var from `ostia serve` into the sandbox — including `HOME`, `USER`, `SSH_*`, any credentials the parent has loaded, and any `OSTIA_*` vars from the operator. `execve` takes an explicit env vector, so the sandbox sees exactly what the profile constructed and nothing else. This is the mechanism behind contract `C-EI2` in `spec/credentials.md` — the sandbox does not inherit parent env.

## Why `/bin/sh` and `-c`

Commands are arbitrary shell strings. Rather than re-implementing a POSIX shell, we bind-mount `/bin/sh` into the namespace and let it handle pipes, redirects, `$(...)`, `&&`, etc. The command string is passed as the single `-c` argument.

This has a known tension with the command matcher (issue #8 Gap 2) — the matcher tries to parse the command string itself for allowlist enforcement, but the shell parses differently from our whitespace-split lexer. The long-term fix is to stop pre-lexing commands and enforce at the `execve` layer (e.g., via PATH bind-mounting). Current state: whitespace-split is wrong for many real shell constructs, and users work around it by writing scripts to the workspace.

## Goblin dependency resolution

Recursive BFS over `DT_NEEDED`:

```rust
fn resolve_all_deps(binary: &Path) -> HashSet<PathBuf> {
    let mut resolved = HashSet::new();
    let mut queue = VecDeque::from([binary.to_owned()]);
    while let Some(next) = queue.pop_front() {
        if !resolved.insert(next.clone()) { continue; }
        let bytes = std::fs::read(&next)?;
        let elf = goblin::elf::Elf::parse(&bytes)?;
        for lib in elf.libraries {
            let path = resolve_soname(lib, &elf.runpaths, &ld_so_cache)?;
            queue.push_back(path);
        }
    }
    resolved
}
```

Caches resolved deps per-profile at profile load time, not per-command. A profile with 10 binaries does the full walk once; command invocations reuse the cached set.

**Edge cases the current implementation handles:**
- `$ORIGIN` in RPATH — resolved against the binary's directory
- Multi-arch paths (`lib`, `lib64`, `x86_64-linux-gnu`) — tried in order
- Nested deps (libA → libB → libC) — BFS to termination
- Dynamic linker (`/lib64/ld-linux-x86-64.so.2`) — always mounted

**Edge cases NOT handled:**
- `dlopen`-loaded libraries at runtime. If a binary dynamically loads a library name we didn't see in DT_NEEDED, the load will fail inside the sandbox. Workaround: add the lib path explicitly to the profile.
- Symlinked binaries pointing to different files. `python3` → `python3.11` is a known class of bug (tracked in launch-checklist).
- Script-based tools that fork into interpreter-specific runtimes. `npm` and `npx` need the Node.js tree available, not just the `npm` binary. Also tracked in launch-checklist.

## Two-step bind-mount pattern

Linux rejects a `mount(source, dest, "", MS_BIND | MS_RDONLY, NULL)` in a single call — the readonly flag is ignored on the first mount. You have to do it in two steps:

```rust
mount(source, dest, "", MS_BIND, NULL)?;
mount(source, dest, "", MS_BIND | MS_REMOUNT | MS_RDONLY, NULL)?;
```

The first `mount` creates the bind mount. The second `mount(..., MS_REMOUNT, ...)` changes it to read-only. Without the remount step, every bind mount is rw.

Applies to: binaries, libraries, `read:` paths, `/bin/sh`, `/dev/*` nodes.

Does NOT apply to: the tmpfs rootfs (created with MS_NOSUID | MS_NODEV, not a bind mount), the workspace (deliberately rw), `/proc` (mounted fresh, not bind-mounted).

## Landlock strategy

Landlock rules are constructed from:

- **Workspace** → `LANDLOCK_ACCESS_FS_READ_FILE | READ_DIR | WRITE_FILE | MAKE_REG | MAKE_DIR | REMOVE_FILE | REMOVE_DIR | REFER`
- **Read paths** → `LANDLOCK_ACCESS_FS_READ_FILE | READ_DIR`
- **Mandatory deny paths** (~/.ssh, ~/.aws, .env, etc.) → NO rule (absence = denied in Landlock)

The ruleset is applied after `pivot_root` so paths are resolved against the new rootfs. If the Landlock ABI is older than expected, we log a warning and proceed with mount-namespace-only isolation. The fallback still covers the binary allowlist (which is the hard security claim) but loses filesystem granularity for writable paths.

## Seccomp filter shape

Deny-list approach via `seccompiler::SeccompFilter`:

```rust
SeccompFilter::new(
    vec![
        (libc::SYS_mount, vec![]),         // unconditional deny
        (libc::SYS_unshare, vec![]),
        (libc::SYS_clone, vec![           // conditional: deny if any namespace flag
            SeccompRule::new(vec![
                SeccompCondition::new(0, Arg32, MaskedEq(0x38000000), 0)?,
            ])?,
        ]),
        (libc::SYS_ptrace, vec![]),
        (libc::SYS_kexec_load, vec![]),
        (libc::SYS_open_by_handle_at, vec![]),
    ],
    SeccompAction::Allow,                 // default: allow
    SeccompAction::Errno(libc::EPERM as u32),
    std::env::consts::ARCH.try_into()?,
)?
```

The BPF program is compiled once per executor and applied to each child process before exec.

## Why not just use bubblewrap?

We did evaluate bubblewrap, firejail, and nsjail. All have the right primitives but come with operational overhead: a separate binary to ship, a different config format to learn, and an extra process layer between ostia and the sandboxed command. Writing the namespace setup in Rust directly means:

- One binary to ship. `cargo install ostia` is the whole dependency tree.
- Config is YAML (not bubblewrap CLI flags).
- Namespace setup is inside the same process that owns the profile, so there's no IPC between "the config layer" and "the enforcement layer."
- Rust's type system enforces that `SandboxExecutor` is constructed from a resolved `Profile`, not from a loose set of strings that bubblewrap would parse.

Cost: Ostia's namespace code is ~1500 lines that wouldn't exist if we shelled out to bubblewrap. But those 1500 lines are the thing that makes the product what it is.

## Container-in-container considerations

When Ostia runs inside a Docker container (primary deployment mode), the nested mount namespace setup works but has known sharp edges:

- **Ubuntu 24.04 AppArmor restriction.** `kernel.apparmor_restrict_unprivileged_userns=1` (default) blocks the `setgroups` write required for nested user namespaces. Workaround documented in issue #2: set the sysctl to 0 on the Colima/host side.
- **`--privileged` flag** on the outer container is currently required for mount namespace operations. Non-privileged nested namespaces work on recent kernels but the minimum capability set is `CAP_SYS_ADMIN` + unrestricted AppArmor profile.
- **Graceful degradation escape hatch.** Not implemented. Proposed `--unsafe-no-sandbox` flag in issue #2 was rejected because it removes the per-call binary allowlist, which is Ostia's core value proposition.

## Streaming output implementation

`execute_streaming(cmd, tx: Sender<Chunk>)` runs the child in a separate thread and forwards line-buffered stdout/stderr to the channel. The sender:

```rust
enum Chunk {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    Exit(i32),
}
```

`execute_streaming_collect` wraps this — it spawns a thread that drains the channel into a `String` buffer per stream and returns an `ExecutionResult { allowed, exit_code, stdout, stderr }` when `Exit` arrives.

The streaming API was added late (V6.5 in the original plan) because the initial design was batch-only, which made long-running commands feel dead for 30+ seconds with no feedback. This is tested by contracts `C-ST1`/`C-ST2`/`C-ST3` (CLI-level) and `C-SA5`/`C-SA6`/`C-SA7`/`C-SA8` (API-level) in `spec/sandbox.md`.
