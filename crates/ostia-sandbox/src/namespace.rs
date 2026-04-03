use std::collections::HashSet;
use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use nix::mount::{mount, umount2, MntFlags, MsFlags};
use nix::sched::{unshare, CloneFlags};
use nix::unistd::{getuid, getgid};

use crate::resolve::ResolvedBinary;

/// Standard symlinks for merged-usr compatibility.
/// On merged-usr systems, /lib -> /usr/lib, /bin -> /usr/bin, etc.
/// We recreate these so that binaries referencing either path will work.
const COMPAT_SYMLINKS: &[(&str, &str)] = &[
    ("/lib", "/usr/lib"),
    ("/lib64", "/usr/lib64"),
    ("/bin", "/usr/bin"),
    ("/sbin", "/usr/sbin"),
];

/// Minimal device nodes that are safe to expose inside the sandbox.
const SAFE_DEVICES: &[&str] = &["/dev/null", "/dev/zero", "/dev/urandom", "/dev/random"];

/// Paths that must NEVER be mounted into the sandbox, regardless of profile config.
/// Checked as suffix matches against canonicalized paths.
const MANDATORY_DENY_SUFFIXES: &[&str] = &[
    ".ssh",
    ".env",
    ".git/hooks",
    ".bashrc",
    ".zshrc",
    ".bash_profile",
    ".profile",
    ".aws",
    ".gnupg",
    ".config/gh",
];

/// Perform a read-only bind mount using the required two-step pattern.
///
/// Linux silently ignores `MS_RDONLY` on the initial bind mount call.
/// To get a truly read-only bind mount, you must first bind, then remount
/// with `MS_BIND | MS_REMOUNT | MS_RDONLY`.
pub fn bind_mount_readonly(source: &Path, target: &Path) -> Result<()> {
    // Step 1: initial bind mount
    mount(
        Some(source),
        target,
        None::<&str>,
        MsFlags::MS_BIND,
        None::<&str>,
    )
    .with_context(|| {
        format!(
            "bind mount failed: {} -> {}",
            source.display(),
            target.display()
        )
    })?;

    // Step 2: remount read-only
    mount(
        None::<&str>,
        target,
        None::<&str>,
        MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY,
        None::<&str>,
    )
    .with_context(|| {
        format!(
            "remount read-only failed: {}",
            target.display()
        )
    })?;

    Ok(())
}

/// Bind mount with best-effort read-only remount.
///
/// In user namespaces, remounting as read-only can fail with EPERM when the
/// source mount is locked (inherited from the parent namespace). This function
/// succeeds with a read-write bind mount in that case — Landlock provides
/// write protection as a defense-in-depth layer.
fn bind_mount_readonly_best_effort(source: &Path, target: &Path) -> Result<()> {
    mount(
        Some(source),
        target,
        None::<&str>,
        MsFlags::MS_BIND,
        None::<&str>,
    )
    .with_context(|| {
        format!(
            "bind mount failed: {} -> {}",
            source.display(),
            target.display()
        )
    })?;

    // Best-effort remount — Landlock enforces write restrictions regardless.
    let _ = mount(
        None::<&str>,
        target,
        None::<&str>,
        MsFlags::MS_BIND | MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY,
        None::<&str>,
    );

    Ok(())
}

/// Perform a read-write bind mount.
fn bind_mount_readwrite(source: &Path, target: &Path) -> Result<()> {
    mount(
        Some(source),
        target,
        None::<&str>,
        MsFlags::MS_BIND,
        None::<&str>,
    )
    .with_context(|| {
        format!(
            "bind mount (rw) failed: {} -> {}",
            source.display(),
            target.display()
        )
    })
}

/// Ensure the parent directory of `path` exists, creating it if necessary.
fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!("failed to create parent directory: {}", parent.display())
        })?;
    }
    Ok(())
}

/// Ensure a file exists as a bind-mount target (creates an empty file).
fn ensure_mount_point(path: &Path) -> Result<()> {
    ensure_parent(path)?;
    if !path.exists() {
        fs::File::create(path).with_context(|| {
            format!("failed to create mount point: {}", path.display())
        })?;
    }
    Ok(())
}

/// Ensure a directory exists as a bind-mount target.
fn ensure_dir_mount_point(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| {
        format!("failed to create directory mount point: {}", path.display())
    })
}

/// Map a host path into the new root, preserving the directory structure.
/// For example, `/usr/bin/git` on the host becomes `<newroot>/usr/bin/git`.
fn target_path(new_root: &Path, host_path: &Path) -> PathBuf {
    // Strip the leading `/` so join works correctly
    let relative = host_path
        .strip_prefix("/")
        .unwrap_or(host_path);
    new_root.join(relative)
}

/// Bind-mount a single file (read-only) from host into the new root.
fn mount_file_ro(new_root: &Path, host_path: &Path) -> Result<()> {
    let target = target_path(new_root, host_path);
    ensure_mount_point(&target)?;
    bind_mount_readonly(host_path, &target)
}

/// Collect all unique paths that need to be bind-mounted for the given binaries.
fn collect_mount_paths(binaries: &[ResolvedBinary]) -> HashSet<PathBuf> {
    let mut paths = HashSet::new();

    for binary in binaries {
        // The binary itself
        paths.insert(binary.path.clone());

        // Its ELF interpreter (e.g. /lib64/ld-linux-x86-64.so.2)
        if let Some(ref interp) = binary.interpreter {
            paths.insert(interp.clone());
        }

        // All shared libraries
        for lib in &binary.libraries {
            paths.insert(lib.clone());
        }
    }

    paths
}

/// Check whether a path matches any mandatory deny suffix.
///
/// Returns true if the path should be blocked from mounting.
fn is_mandatory_deny(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    for suffix in MANDATORY_DENY_SUFFIXES {
        if path_str.ends_with(suffix) || path_str.contains(&format!("{}/", suffix)) {
            return true;
        }
    }
    false
}

/// Set up the sandbox mount namespace.
///
/// This creates a minimal filesystem with only the whitelisted binaries,
/// their ELF interpreters, and shared libraries visible. Everything else
/// is hidden from the sandboxed process.
///
/// # Arguments
/// - `binaries` — resolved whitelisted binaries and their deps
/// - `workspace` — read-write workspace directory
/// - `read_paths` — additional directories/files to mount read-only
///
/// # Requirements
/// - Must be called with `CAP_SYS_ADMIN` (typically as root or in a user namespace).
/// - The calling process should not have other threads (unshare requirement).
///
/// # Safety
/// After this function returns successfully, the calling process is in a
/// pivot_root'd namespace. The old root has been unmounted and detached.
pub fn setup_sandbox_namespace(
    binaries: &[ResolvedBinary],
    workspace: Option<&Path>,
    read_paths: &[PathBuf],
) -> Result<()> {
    // ── 1. Create user + mount namespaces ────────────────────────────
    // We need CLONE_NEWUSER to gain CAP_SYS_ADMIN inside the namespace,
    // which lets us create mount namespaces without root on the host.
    let uid = getuid();
    let gid = getgid();

    unshare(CloneFlags::CLONE_NEWUSER | CloneFlags::CLONE_NEWNS)
        .context("unshare(CLONE_NEWUSER | CLONE_NEWNS) failed — check /proc/sys/kernel/unprivileged_userns_clone")?;

    // Write uid_map and gid_map to map our host uid/gid to root inside the namespace.
    // This must happen before any mount operations.
    fs::write("/proc/self/setgroups", "deny")
        .context("failed to write /proc/self/setgroups")?;
    fs::write("/proc/self/uid_map", format!("0 {} 1", uid))
        .context("failed to write uid_map")?;
    fs::write("/proc/self/gid_map", format!("0 {} 1", gid))
        .context("failed to write gid_map")?;

    // ── 2. Kill mount propagation so nothing leaks back to the host ──
    mount(
        None::<&str>,
        "/",
        None::<&str>,
        MsFlags::MS_REC | MsFlags::MS_PRIVATE,
        None::<&str>,
    )
    .context("failed to set mount propagation to private")?;

    // ── 3. Create a tmpfs to serve as our new root ───────────────────
    let new_root_path = PathBuf::from("/tmp/ostia-sandbox-root");
    fs::create_dir_all(&new_root_path)
        .context("failed to create directory for new root")?;

    mount(
        Some("tmpfs"),
        &new_root_path,
        Some("tmpfs"),
        MsFlags::MS_NOSUID | MsFlags::MS_NODEV,
        Some("size=64m"),
    )
    .context("failed to mount tmpfs as new root")?;

    // ── 4. Bind-mount all resolved binaries and their deps ───────────
    let mount_paths = collect_mount_paths(binaries);

    for host_path in &mount_paths {
        if !host_path.exists() {
            continue;
        }
        mount_file_ro(&new_root_path, host_path)
            .with_context(|| format!("failed to mount {}", host_path.display()))?;
    }

    // ── 5. Create merged-usr compatibility symlinks ──────────────────
    for &(link, target) in COMPAT_SYMLINKS {
        let link_in_new = new_root_path.join(link.trim_start_matches('/'));
        let target_in_new = new_root_path.join(target.trim_start_matches('/'));

        // Only create the symlink if:
        // - The target directory exists in our new root
        // - The link path doesn't already exist (might have been created by a mount)
        if target_in_new.exists() && !link_in_new.exists() {
            ensure_parent(&link_in_new)?;
            std::os::unix::fs::symlink(target, &link_in_new).with_context(|| {
                format!("failed to create compat symlink: {} -> {}", link, target)
            })?;
        }
    }

    // ── 6. Ensure /bin/sh is always available ────────────────────────
    // /bin/sh is required for command execution (we exec via sh -c).
    // On merged-usr systems, /bin/sh -> bash, and /usr/bin/sh -> bash.
    // We need the real bash binary AND a sh mount point at /usr/bin/sh.
    {
        let sh_path = Path::new("/bin/sh");
        let real_sh = fs::canonicalize(sh_path)
            .context("/bin/sh not found on host — cannot create sandbox")?;

        // Mount the real shell binary if not already mounted
        let real_sh_target = target_path(&new_root_path, &real_sh);
        if !real_sh_target.exists() {
            mount_file_ro(&new_root_path, &real_sh)
                .with_context(|| format!("failed to mount shell ({})", real_sh.display()))?;
        }

        // Create /usr/bin/sh as a mount of the same binary (so /bin/sh -> /usr/bin/sh works)
        let sh_in_usr = new_root_path.join("usr/bin/sh");
        if !sh_in_usr.exists() {
            ensure_mount_point(&sh_in_usr)?;
            bind_mount_readonly(&real_sh, &sh_in_usr)?;
        }

        // Resolve sh's dependencies if not already covered
        if let Ok(sh_resolved) = crate::resolve::resolve_binary_deps(&real_sh) {
            if let Some(ref interp) = sh_resolved.interpreter {
                if interp.exists() {
                    let interp_target = target_path(&new_root_path, interp);
                    if !interp_target.exists() {
                        mount_file_ro(&new_root_path, interp)?;
                    }
                }
            }
            for lib in &sh_resolved.libraries {
                if lib.exists() {
                    let lib_target = target_path(&new_root_path, lib);
                    if !lib_target.exists() {
                        mount_file_ro(&new_root_path, lib)?;
                    }
                }
            }
        }
    }

    // ── 7. Mount /proc ────────────────────────────────────────────────
    // In a user namespace without CLONE_NEWPID, mounting a fresh procfs
    // requires the host /proc to be accessible. Try a fresh mount first,
    // fall back to bind-mount, and finally skip if neither works.
    let proc_path = new_root_path.join("proc");
    ensure_dir_mount_point(&proc_path)?;

    let proc_mounted = mount(
        Some("proc"),
        &proc_path,
        Some("proc"),
        MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC | MsFlags::MS_NODEV,
        None::<&str>,
    )
    .is_ok();

    if !proc_mounted {
        // Try bind-mounting the host /proc read-only
        let _ = bind_mount_readonly(Path::new("/proc"), &proc_path);
        // If this also fails, continue without /proc — most CLI tools don't need it
    }

    // ── 8. Create minimal /dev ────────────────────────────────────────
    // Device nodes can't be created in a user namespace, so we bind-mount
    // /dev from the host. This is safe because the mount namespace prevents
    // access to anything not explicitly mounted.
    let dev_path = new_root_path.join("dev");
    ensure_dir_mount_point(&dev_path)?;

    // Try a tmpfs first, fall back to bind-mounting host /dev
    let dev_mounted = mount(
        Some("tmpfs"),
        &dev_path,
        Some("tmpfs"),
        MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC,
        Some("size=64k,mode=0755"),
    )
    .is_ok();

    if dev_mounted {
        // Mount individual safe devices
        for device in SAFE_DEVICES {
            let host_dev = Path::new(device);
            let sandbox_dev = new_root_path.join(device.trim_start_matches('/'));
            if host_dev.exists() {
                ensure_mount_point(&sandbox_dev)?;
                // /dev/null must be writable — many tools redirect output there.
                // Other safe devices (urandom, zero) are read-only.
                if *device == "/dev/null" {
                    let _ = bind_mount_readwrite(host_dev, &sandbox_dev);
                } else {
                    let _ = bind_mount_readonly(host_dev, &sandbox_dev);
                }
            }
        }
    } else {
        // Bind-mount host /dev read-only as fallback
        let _ = bind_mount_readonly(Path::new("/dev"), &dev_path);
    }

    // ── 9. Bind-mount the workspace (read-write) ─────────────────────
    if let Some(ws) = workspace {
        let ws = fs::canonicalize(ws)
            .with_context(|| format!("workspace path not found: {}", ws.display()))?;
        let ws_target = target_path(&new_root_path, &ws);
        ensure_dir_mount_point(&ws_target)?;
        bind_mount_readwrite(&ws, &ws_target)?;
    }

    // ── 10. Bind-mount additional read paths (read-only) ──────────
    for read_path in read_paths {
        let canonical = match fs::canonicalize(read_path) {
            Ok(p) => p,
            Err(_) => continue, // path doesn't exist on host — skip silently
        };

        // Mandatory deny paths are never mounted, even if listed in config.
        if is_mandatory_deny(&canonical) {
            continue;
        }

        let rp_target = target_path(&new_root_path, &canonical);
        if canonical.is_dir() {
            ensure_dir_mount_point(&rp_target)?;
        } else {
            ensure_mount_point(&rp_target)?;
        }
        bind_mount_readonly_best_effort(&canonical, &rp_target)
            .with_context(|| format!("failed to mount read path: {}", canonical.display()))?;
    }

    // ── 11. pivot_root into the new filesystem ───────────────────────
    // Create a directory to stash the old root during pivot
    let old_root = new_root_path.join("old_root");
    ensure_dir_mount_point(&old_root)?;

    let new_root_cstr = CString::new(new_root_path.as_os_str().as_bytes())
        .context("new root path contains null bytes")?;
    let old_root_cstr = CString::new(old_root.as_os_str().as_bytes())
        .context("old root path contains null bytes")?;

    // pivot_root is not wrapped by the nix crate — use libc::syscall directly
    let ret = unsafe {
        libc::syscall(
            libc::SYS_pivot_root,
            new_root_cstr.as_ptr(),
            old_root_cstr.as_ptr(),
        )
    };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        anyhow::bail!("pivot_root failed: {}", err);
    }

    // ── 11. chdir to new root ────────────────────────────────────────
    nix::unistd::chdir("/")
        .context("failed to chdir(\"/\") after pivot_root")?;

    // ── 12. Unmount the old root ─────────────────────────────────────
    umount2("/old_root", MntFlags::MNT_DETACH)
        .context("failed to unmount old root")?;

    // Remove the now-empty old_root mount point
    let _ = fs::remove_dir("/old_root");

    // ── 13. Prevent privilege escalation ──────────────────────────────
    let ret = unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        anyhow::bail!("prctl(PR_SET_NO_NEW_PRIVS) failed: {}", err);
    }

    Ok(())
}
