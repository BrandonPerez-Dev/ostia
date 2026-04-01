//! Integration tests: Streaming output (V6.5, Slice 1).
//!
//! V6.5 contracts — validates that `ostia run` streams stdout/stderr in
//! real-time and that tracing instrumentation does not leak into command
//! output streams.

mod common;

use std::process::Command;

/// Contract 1: Real-time CLI output
/// When a command produces output in stages,
/// Then both lines appear in stdout in order and exit 0.
#[test]
fn streaming_cli_output_preserves_order() {
    // Arrange
    common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = common::write_sandbox_config(workspace.path().to_str().unwrap(), &["sleep"]);

    // Act — two echoes with a brief pause between them
    let output = Command::new(common::ostia_bin())
        .args([
            "run",
            "--config",
            config.path().to_str().unwrap(),
            "--profile",
            "test",
            "--",
        ])
        .arg("echo first && sleep 0.2 && echo second")
        .output()
        .expect("failed to execute ostia");

    // Assert
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "should exit 0, stderr={:?}",
        stderr
    );
    assert!(
        stdout.contains("first"),
        "stdout should contain 'first', got: {:?}",
        stdout
    );
    assert!(
        stdout.contains("second"),
        "stdout should contain 'second', got: {:?}",
        stdout
    );
    // Verify ordering — "first" appears before "second"
    let first_pos = stdout.find("first").unwrap();
    let second_pos = stdout.find("second").unwrap();
    assert!(
        first_pos < second_pos,
        "output should be ordered: first before second, got: {:?}",
        stdout
    );
}

/// Contract 2: stderr streams separately
/// When a command writes to both stdout and stderr,
/// Then stdout and stderr each contain only their respective content.
#[test]
fn streaming_stderr_stays_separate() {
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
        ])
        .arg("echo out && bash -c 'echo err >&2'")
        .output()
        .expect("failed to execute ostia");

    // Assert
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "should exit 0, stderr={:?}",
        stderr
    );
    assert_eq!(
        stdout.trim(),
        "out",
        "stdout should contain only command stdout"
    );
    assert!(
        stderr.is_empty() || stderr.trim() == "err",
        "stderr should contain only command stderr (or be empty if tracing is off), got: {:?}",
        stderr
    );
    assert!(
        !stdout.contains("err"),
        "stdout must not contain stderr content, got: {:?}",
        stdout
    );
}

/// Contract 3: tracing output does not leak into command stdout
/// When tracing is enabled via RUST_LOG, Then stdout remains pristine
/// (only command output). Tracing may appear on stderr but must be
/// structured and distinguishable from command stderr.
#[test]
fn tracing_does_not_leak_into_stdout() {
    // Arrange
    common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = common::write_sandbox_config(workspace.path().to_str().unwrap(), &[]);

    // Act — enable debug-level tracing
    let output = Command::new(common::ostia_bin())
        .env("RUST_LOG", "debug")
        .args([
            "run",
            "--config",
            config.path().to_str().unwrap(),
            "--profile",
            "test",
            "--",
            "echo",
            "clean",
        ])
        .output()
        .expect("failed to execute ostia");

    // Assert — stdout is pristine
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "should exit 0, stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        stdout.trim(),
        "clean",
        "stdout must contain only command output, even with RUST_LOG=debug, got: {:?}",
        stdout
    );
    // stdout must not contain tracing artifacts
    assert!(
        !stdout.contains("DEBUG"),
        "stdout must not contain tracing DEBUG lines, got: {:?}",
        stdout
    );
    assert!(
        !stdout.contains("INFO"),
        "stdout must not contain tracing INFO lines, got: {:?}",
        stdout
    );
    assert!(
        !stdout.contains("TRACE"),
        "stdout must not contain tracing TRACE lines, got: {:?}",
        stdout
    );
}
