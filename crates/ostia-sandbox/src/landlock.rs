use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use landlock::{
    Access, AccessFs, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreatedAttr,
    RulesetStatus, ABI,
};

/// The Landlock ABI version we target.
/// V3 (Linux 6.2) adds Truncate, which is required for O_TRUNC in write ops.
/// BestEffort mode downgrades gracefully on older kernels.
const TARGET_ABI: ABI = ABI::V3;

/// Apply Landlock filesystem restrictions inside the sandbox.
///
/// This is a defense-in-depth layer on top of mount namespace isolation.
/// After pivot_root, the sandbox root is a writable tmpfs. Landlock constrains
/// writes to only the workspace directory (if configured), preventing the
/// sandboxed process from modifying anything else in the namespace.
///
/// # Paths (all post-pivot_root)
///
/// - `/` gets read + execute access (so mounted binaries can be read and run)
/// - `workspace` gets full read + write + execute access
/// - `read_paths` get read + execute access (no writes)
///
/// # Compatibility
///
/// Uses BestEffort mode (the default). On kernels without Landlock support
/// (< 5.13), this function succeeds silently — the mount namespace is still
/// the primary security boundary.
pub fn apply_landlock_restrictions(
    workspace: Option<&Path>,
    read_paths: &[PathBuf],
) -> Result<()> {
    let read_execute = AccessFs::from_read(TARGET_ABI) | AccessFs::Execute;

    let mut ruleset = Ruleset::default()
        .handle_access(AccessFs::from_all(TARGET_ABI))
        .context("landlock: failed to handle access")?
        .create()
        .context("landlock: failed to create ruleset")?;

    // Grant read + execute on the entire sandbox root.
    // This allows the process to read and execute all mounted binaries.
    if let Ok(root_fd) = PathFd::new("/") {
        ruleset = ruleset
            .add_rule(PathBeneath::new(root_fd, read_execute))
            .context("landlock: failed to add root read rule")?;
    }

    // Grant write access to /dev/null — many CLI tools redirect output there.
    if let Ok(null_fd) = PathFd::new("/dev/null") {
        ruleset = ruleset
            .add_rule(PathBeneath::new(null_fd, AccessFs::from_all(TARGET_ABI)))
            .context("landlock: failed to add /dev/null rule")?;
    }

    // Grant full access to the workspace directory.
    if let Some(ws) = workspace {
        if ws.exists() {
            if let Ok(ws_fd) = PathFd::new(ws) {
                ruleset = ruleset
                    .add_rule(PathBeneath::new(ws_fd, AccessFs::from_all(TARGET_ABI)))
                    .context("landlock: failed to add workspace rule")?;
            }
        }
    }

    // Grant read + execute on additional read paths.
    // These are already bind-mounted read-only by the namespace, but
    // Landlock provides defense-in-depth by also blocking writes at the LSM level.
    for read_path in read_paths {
        if read_path.exists() {
            if let Ok(rp_fd) = PathFd::new(read_path) {
                ruleset = ruleset
                    .add_rule(PathBeneath::new(rp_fd, read_execute))
                    .context("landlock: failed to add read path rule")?;
            }
        }
    }

    let status = ruleset
        .restrict_self()
        .context("landlock: failed to restrict self")?;

    match status.ruleset {
        RulesetStatus::FullyEnforced => {}
        RulesetStatus::PartiallyEnforced => {
            // Some access rights not supported by this kernel — still useful
        }
        RulesetStatus::NotEnforced => {
            // Kernel doesn't support Landlock — mount namespace is the boundary
        }
    }

    Ok(())
}
