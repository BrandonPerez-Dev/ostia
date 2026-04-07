//! Integration tests: CLI produces clean output (no debug noise).
//!
//! V1 contracts — validates that `ostia run` produces only the sandboxed
//! command's output on stdout/stderr with no framework-injected lines.
//! Agents parse stdout; any injection corrupts the data stream.

mod common;

use std::process::Command;

/// Contract 1: Clean stdout
/// When a command produces stdout, Then ostia outputs exactly the command's
/// stdout with no injected lines, empty stderr, and exit 0.
#[test]
fn stdout_contains_only_command_output() {
    // Arrange
    common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = common::write_sandbox_config(workspace.path().to_str().unwrap(), &[]);

    // Act
    let output = Command::new(common::ostia_bin())
        .args([
            "run",
            "--config",
            config.path().to_str().unwrap(),
            "--profile",
            "test",
            "--",
            "echo",
            "hello",
        ])
        .output()
        .expect("failed to execute ostia");

    // Assert
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(stdout, "hello\n", "stdout should contain only command output");
    assert!(
        stderr.is_empty(),
        "stderr should be empty for a clean echo, got: {:?}",
        stderr
    );
    assert!(output.status.success(), "exit code should be 0");
}

/// Contract 2: Clean stderr passthrough
/// When a command writes to stderr and exits non-zero, Then ostia passes
/// through the command's stderr without injecting debug/framework lines.
#[test]
fn stderr_passthrough_has_no_injected_lines() {
    // Arrange
    common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = common::write_sandbox_config(workspace.path().to_str().unwrap(), &[]);

    // Act — single-quoted compound protects `;` from matcher splitting
    let output = Command::new(common::ostia_bin())
        .args([
            "run",
            "--config",
            config.path().to_str().unwrap(),
            "--profile",
            "test",
            "--",
        ])
        .arg("bash -c 'echo err >&2; exit 1'")
        .output()
        .expect("failed to execute ostia");

    // Assert
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stdout.is_empty(),
        "stdout should be empty when command only writes to stderr, got: {:?}",
        stdout
    );
    assert!(
        stderr.contains("err"),
        "stderr should contain the command's error output, got: {:?}",
        stderr
    );
    assert!(
        !stderr.contains("ostia-debug"),
        "stderr must not contain ostia-debug lines, got: {:?}",
        stderr
    );
    assert!(
        !stderr.contains("ostia:"),
        "stderr must not contain ostia: prefixed lines, got: {:?}",
        stderr
    );
    assert!(
        !stderr.contains("warning:"),
        "stderr must not contain warning: lines, got: {:?}",
        stderr
    );
    assert!(
        !output.status.success(),
        "exit code should be non-zero for a failing command"
    );
}

/// Contract 3: Mixed stdout + stderr separation
/// When a command writes to both stdout and stderr, Then ostia keeps the
/// streams cleanly separated with no cross-contamination or injected lines.
#[test]
fn mixed_stdout_stderr_stay_separated() {
    // Arrange
    common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = common::write_sandbox_config(workspace.path().to_str().unwrap(), &[]);

    // Act — echo to stdout, then bash -c to write to stderr; && split is
    // allowed by matcher (both echo and bash are whitelisted)
    let output = Command::new(common::ostia_bin())
        .args([
            "run",
            "--config",
            config.path().to_str().unwrap(),
            "--profile",
            "test",
            "--",
        ])
        .arg("echo out && bash -c 'echo err >&2'")
        .output()
        .expect("failed to execute ostia");

    // Assert — stdout
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("out"),
        "stdout should contain the command's stdout output, got: {:?}",
        stdout
    );
    assert!(
        !stdout.contains("err"),
        "stdout must not contain stderr content (cross-contamination), got: {:?}",
        stdout
    );
    assert!(
        !stdout.contains("ostia-debug"),
        "stdout must not contain ostia-debug lines, got: {:?}",
        stdout
    );
    assert!(
        !stdout.contains("ostia:"),
        "stdout must not contain ostia: prefixed lines, got: {:?}",
        stdout
    );

    // Assert — stderr
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("err"),
        "stderr should contain the command's stderr output, got: {:?}",
        stderr
    );
    assert!(
        !stderr.contains("out"),
        "stderr must not contain stdout content (cross-contamination), got: {:?}",
        stderr
    );
    assert!(
        !stderr.contains("ostia-debug"),
        "stderr must not contain ostia-debug lines, got: {:?}",
        stderr
    );
    assert!(
        !stderr.contains("ostia:"),
        "stderr must not contain ostia: prefixed lines, got: {:?}",
        stderr
    );

    // Assert — exit code
    assert!(
        output.status.success(),
        "exit code should be 0 when command exits 0, got: {:?}",
        output.status
    );
}
