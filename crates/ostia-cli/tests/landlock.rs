//! Integration tests: Landlock filesystem enforcement (V2).
//!
//! V2 contracts — validates that Landlock restricts filesystem access:
//! workspace gets full access, read_paths are read-only, mandatory deny
//! paths are blocked even when in read_paths, and sensitive paths are
//! not visible in the mount namespace.

mod common;

use std::fs;
use std::process::Command;

/// Write a config with additional read paths.
fn write_config_with_read_paths(workspace: &str, read_paths: &[&str]) -> tempfile::NamedTempFile {
    let read_list = read_paths
        .iter()
        .map(|p| format!("        - {}", p))
        .collect::<Vec<_>>()
        .join("\n");
    let config = format!(
        r#"bundles:
  baseline:
    binaries: [sh, bash, echo, cat, ls]

profiles:
  landlock-test:
    bundles: [baseline]
    filesystem:
      workspace: {workspace}
      read:
{read_list}
"#
    );
    let mut f = tempfile::NamedTempFile::new().expect("create temp config");
    std::io::Write::write_all(&mut f, config.as_bytes()).expect("write config");
    f
}

/// Contract 1: Workspace write access
/// When a command writes and reads back inside the workspace,
/// Then the write succeeds, stdout contains the written content, and exit 0.
#[test]
fn write_to_workspace_succeeds() {
    common::assert_user_namespaces();
    common::assert_landlock();

    let workspace = tempfile::tempdir().expect("create workspace");
    let ws_path = workspace.path().to_str().unwrap();
    let config = common::write_sandbox_config(ws_path, &[]);

    let shell_cmd = format!("echo hello > {}/testfile && cat {}/testfile", ws_path, ws_path);
    let output = Command::new(common::ostia_bin())
        .args([
            "run",
            "--config",
            config.path().to_str().unwrap(),
            "--profile",
            "test",
            "--",
        ])
        .arg(&shell_cmd)
        .output()
        .expect("failed to execute ostia");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stdout.trim(), "hello", "should read back what was written");
    assert!(
        stderr.is_empty(),
        "stderr should be empty for a successful workspace write, got: {:?}",
        stderr
    );
    assert!(
        output.status.success(),
        "write to workspace should succeed, exit={:?}",
        output.status
    );
}

/// Contract 2: Write outside workspace blocked
/// When a command writes outside the workspace directory,
/// Then Landlock denies the write with Permission denied and exit non-zero.
#[test]
fn write_outside_workspace_fails_with_permission_error() {
    common::assert_user_namespaces();
    common::assert_landlock();

    let workspace = tempfile::tempdir().expect("create workspace");
    let config = common::write_sandbox_config(workspace.path().to_str().unwrap(), &[]);

    let output = Command::new(common::ostia_bin())
        .args([
            "run",
            "--config",
            config.path().to_str().unwrap(),
            "--profile",
            "test",
            "--",
            "echo pwned > /tmp/ostia-landlock-outside",
        ])
        .output()
        .expect("failed to execute ostia");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.is_empty(),
        "stdout should be empty for a blocked write, got: {:?}",
        stdout
    );
    assert!(
        stderr.contains("Permission denied") || stderr.contains("Read-only file system"),
        "failure should be permission-related, got stderr: {:?}",
        stderr
    );
    assert!(
        !output.status.success(),
        "write outside workspace should fail"
    );
}

/// Contract 3a: Read path — read succeeds
/// When a command reads from a configured read path,
/// Then the read succeeds and stdout contains the file contents.
#[test]
fn read_path_read_succeeds() {
    common::assert_user_namespaces();
    common::assert_landlock();

    let read_dir = tempfile::tempdir().expect("create read dir");
    let read_file = read_dir.path().join("data.txt");
    fs::write(&read_file, "read-only-content").expect("write read file");

    let workspace = tempfile::tempdir().expect("create workspace");
    let config = write_config_with_read_paths(
        workspace.path().to_str().unwrap(),
        &[read_dir.path().to_str().unwrap()],
    );

    let shell_cmd = format!("cat {}/data.txt", read_dir.path().display());
    let output = Command::new(common::ostia_bin())
        .args([
            "run",
            "--config",
            config.path().to_str().unwrap(),
            "--profile",
            "landlock-test",
            "--",
        ])
        .arg(&shell_cmd)
        .output()
        .expect("failed to execute ostia");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stdout.trim(),
        "read-only-content",
        "should read file from read path"
    );
    assert!(
        stderr.is_empty(),
        "stderr should be empty for a successful read, got: {:?}",
        stderr
    );
    assert!(
        output.status.success(),
        "reading from additional read path should succeed, exit={:?}",
        output.status
    );
}

/// Contract 3b: Read path — write fails
/// When a command writes to a configured read path,
/// Then Landlock denies the write with Permission denied and exit non-zero.
#[test]
fn read_path_write_fails() {
    common::assert_user_namespaces();
    common::assert_landlock();

    let read_dir = tempfile::tempdir().expect("create read dir");
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = write_config_with_read_paths(
        workspace.path().to_str().unwrap(),
        &[read_dir.path().to_str().unwrap()],
    );

    let shell_cmd = format!("echo pwned > {}/hacked.txt", read_dir.path().display());
    let output = Command::new(common::ostia_bin())
        .args([
            "run",
            "--config",
            config.path().to_str().unwrap(),
            "--profile",
            "landlock-test",
            "--",
        ])
        .arg(&shell_cmd)
        .output()
        .expect("failed to execute ostia");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.is_empty(),
        "stdout should be empty for a blocked write, got: {:?}",
        stdout
    );
    assert!(
        stderr.contains("Permission denied") || stderr.contains("Read-only file system"),
        "failure should be permission-related, got stderr: {:?}",
        stderr
    );
    assert!(
        !output.status.success(),
        "writing to read-only path should fail"
    );
}

/// Contract 4: Mandatory deny paths blocked
/// When .ssh is in both mandatory deny and read_paths,
/// Then deny wins — the read is blocked and exit is non-zero.
#[test]
fn mandatory_deny_path_is_blocked() {
    common::assert_user_namespaces();
    common::assert_landlock();

    // Create our own .ssh directory — no dependency on host ~/.ssh.
    let fake_home = tempfile::tempdir().expect("create fake home");
    let fake_ssh = fake_home.path().join(".ssh");
    fs::create_dir(&fake_ssh).expect("create .ssh dir");
    fs::write(fake_ssh.join("id_rsa"), "fake-secret-key").expect("write fake key");

    let workspace = tempfile::tempdir().expect("create workspace");
    let config = write_config_with_read_paths(
        workspace.path().to_str().unwrap(),
        &[fake_ssh.to_str().unwrap()],
    );

    let shell_cmd = format!("cat {}/id_rsa", fake_ssh.display());
    let output = Command::new(common::ostia_bin())
        .args([
            "run",
            "--config",
            config.path().to_str().unwrap(),
            "--profile",
            "landlock-test",
            "--",
        ])
        .arg(&shell_cmd)
        .output()
        .expect("failed to execute ostia");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.is_empty(),
        "stdout should be empty when deny path is blocked, got: {:?}",
        stdout
    );
    assert!(
        !output.status.success(),
        "reading mandatory deny path (.ssh) should fail even when listed in read paths, stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Contract 5: Sensitive paths not visible in sandbox
/// When a command reads /etc/shadow inside the sandbox,
/// Then the file does not exist (mount namespace hides it) — "No such file",
/// not "Permission denied".
#[test]
fn sensitive_paths_not_visible_in_sandbox() {
    // No assert_landlock() — this tests mount namespace isolation, not Landlock.
    common::assert_user_namespaces();

    let workspace = tempfile::tempdir().expect("create workspace");
    let config = common::write_sandbox_config(workspace.path().to_str().unwrap(), &[]);

    let output = Command::new(common::ostia_bin())
        .args([
            "run",
            "--config",
            config.path().to_str().unwrap(),
            "--profile",
            "test",
            "--",
            "cat",
            "/etc/shadow",
        ])
        .output()
        .expect("failed to execute ostia");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.is_empty(),
        "stdout should be empty when file doesn't exist, got: {:?}",
        stdout
    );
    assert!(
        stderr.contains("No such file") || stderr.contains("not found"),
        "should be 'No such file' (mount namespace hides it), not 'Permission denied', got: {:?}",
        stderr
    );
    assert!(
        !stderr.contains("Permission denied"),
        "should NOT be 'Permission denied' — file shouldn't exist in the namespace, got: {:?}",
        stderr
    );
    assert!(
        !output.status.success(),
        "reading /etc/shadow should fail (not visible in sandbox)"
    );
}
