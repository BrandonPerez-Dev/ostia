/// Shared test infrastructure for integration tests.
///
/// All sandbox integration tests require Linux user namespaces.
/// Tests must NOT silently skip — if the capability is missing, they fail.

use std::path::PathBuf;

pub fn ostia_bin() -> PathBuf {
    env!("CARGO_BIN_EXE_ostia").into()
}

pub fn assert_user_namespaces() {
    let available = std::fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone")
        .map(|s| s.trim() == "1")
        // If the file doesn't exist, the kernel doesn't restrict them (most modern kernels)
        .unwrap_or(true);

    assert!(
        available,
        "unprivileged user namespaces are required — check /proc/sys/kernel/unprivileged_userns_clone"
    );
}

pub fn assert_landlock() {
    let available = std::fs::read_to_string("/sys/kernel/security/landlock/status")
        .map(|_| true)
        .unwrap_or_else(|_| {
            let uname = std::process::Command::new("uname").arg("-r").output().ok();
            uname
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|v| {
                    let parts: Vec<u32> = v
                        .trim()
                        .split('.')
                        .take(2)
                        .filter_map(|p| p.parse().ok())
                        .collect();
                    parts.len() >= 2 && (parts[0] > 5 || (parts[0] == 5 && parts[1] >= 13))
                })
                .unwrap_or(false)
        });

    assert!(
        available,
        "Landlock LSM is required — kernel >= 5.13 with CONFIG_SECURITY_LANDLOCK=y"
    );
}

/// Write a temporary config file with a workspace-enabled profile.
pub fn write_sandbox_config(workspace: &str, extra_binaries: &[&str]) -> tempfile::NamedTempFile {
    let mut all_binaries = vec!["sh", "bash", "echo", "cat", "ls"];
    all_binaries.extend_from_slice(extra_binaries);
    let bins = all_binaries
        .iter()
        .map(|b| format!("{}", b))
        .collect::<Vec<_>>()
        .join(", ");

    let config = format!(
        r#"bundles:
  baseline:
    binaries: [{bins}]

profiles:
  test:
    bundles: [baseline]
    filesystem:
      workspace: {workspace}
"#
    );
    let mut f = tempfile::NamedTempFile::new().expect("create temp config");
    std::io::Write::write_all(&mut f, config.as_bytes()).expect("write config");
    f
}
