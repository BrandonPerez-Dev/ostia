//! Integration tests: Built-in bundles + graceful degradation (V6 + VR).
//!
//! Verifies that profiles can reference built-in bundles without defining
//! them in config, and that missing binaries degrade gracefully.

mod common;

use std::process::Command;

// --- Built-in bundle resolution ---

#[test]
fn builtin_baseline_resolves_without_config_definition() {
    // Arrange — config references "baseline" without defining it
    let config_yaml = r#"profiles:
  test:
    bundles: [baseline]
"#;
    let mut f = tempfile::NamedTempFile::new().expect("create temp config");
    std::io::Write::write_all(&mut f, config_yaml.as_bytes()).expect("write config");

    // Act
    let output = Command::new(common::ostia_bin())
        .args(["check", "--config", f.path().to_str().unwrap(), "--profile", "test"])
        .output()
        .expect("failed to execute ostia");

    // Assert
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "check should succeed with built-in baseline, stderr={:?}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.contains("echo"), "baseline should include echo");
    assert!(stdout.contains("cat"), "baseline should include cat");
    assert!(stdout.contains("ls"), "baseline should include ls");
}

#[test]
fn builtin_git_read_resolves_without_config_definition() {
    // Arrange
    let config_yaml = r#"profiles:
  test:
    bundles: [baseline, git-read]
"#;
    let mut f = tempfile::NamedTempFile::new().expect("create temp config");
    std::io::Write::write_all(&mut f, config_yaml.as_bytes()).expect("write config");

    // Act
    let output = Command::new(common::ostia_bin())
        .args(["check", "--config", f.path().to_str().unwrap(), "--profile", "test"])
        .output()
        .expect("failed to execute ostia");

    // Assert
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "check should succeed with built-in git-read, stderr={:?}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.contains("git"), "git-read should include git binary");
}

#[test]
fn config_bundle_overrides_builtin() {
    // Arrange — config defines "baseline" with only echo, overriding the built-in
    let config_yaml = r#"bundles:
  baseline:
    binaries: [echo]

profiles:
  test:
    bundles: [baseline]
"#;
    let mut f = tempfile::NamedTempFile::new().expect("create temp config");
    std::io::Write::write_all(&mut f, config_yaml.as_bytes()).expect("write config");

    // Act
    let output = Command::new(common::ostia_bin())
        .args(["check", "--config", f.path().to_str().unwrap(), "--profile", "test"])
        .output()
        .expect("failed to execute ostia");

    // Assert
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(stdout.contains("echo"), "should have echo");
    assert!(!stdout.contains("cat"), "config override should NOT include built-in's cat");
}

#[test]
fn builtin_bundle_executes_in_sandbox() {
    // Arrange
    common::assert_user_namespaces();

    let workspace = tempfile::tempdir().expect("create workspace");
    let config = format!(
        r#"profiles:
  test:
    bundles: [baseline]
    filesystem:
      workspace: {}
"#,
        workspace.path().to_str().unwrap()
    );
    let mut f = tempfile::NamedTempFile::new().expect("create temp config");
    std::io::Write::write_all(&mut f, config.as_bytes()).expect("write config");

    // Act
    let output = Command::new(common::ostia_bin())
        .args([
            "run", "--config", f.path().to_str().unwrap(),
            "--profile", "test", "--",
            "echo hello-from-builtin",
        ])
        .output()
        .expect("failed to execute ostia");

    // Assert
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "sandbox run with built-in bundle should work, stderr={:?}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(stdout.trim(), "hello-from-builtin");
}

#[test]
fn unknown_bundle_produces_error() {
    // Arrange
    let config_yaml = r#"profiles:
  test:
    bundles: [nonexistent-bundle-xyz]
"#;
    let mut f = tempfile::NamedTempFile::new().expect("create temp config");
    std::io::Write::write_all(&mut f, config_yaml.as_bytes()).expect("write config");

    // Act
    let output = Command::new(common::ostia_bin())
        .args(["check", "--config", f.path().to_str().unwrap(), "--profile", "test"])
        .output()
        .expect("failed to execute ostia");

    // Assert
    assert!(!output.status.success(), "unknown bundle should produce an error");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not found"), "error should mention not found, got: {:?}", stderr);
}

// --- Graceful degradation (C1, C2) ---

#[test]
fn missing_binary_check_warns_without_crash() {
    // Arrange — config with one real binary and one nonexistent
    let config_yaml = r#"bundles:
  test-bundle:
    binaries: [echo, nonexistent-xyz-binary]

profiles:
  test:
    bundles: [test-bundle]
"#;
    let mut f = tempfile::NamedTempFile::new().expect("create temp config");
    std::io::Write::write_all(&mut f, config_yaml.as_bytes()).expect("write config");

    // Act
    let output = Command::new(common::ostia_bin())
        .args(["check", "--config", f.path().to_str().unwrap(), "--profile", "test"])
        .output()
        .expect("failed to execute ostia");

    // Assert — exits 0, shows [missing] for bad binary, [found] for good
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "check should succeed even with missing binary, stderr={:?}", String::from_utf8_lossy(&output.stderr));
    assert!(
        stdout.lines().any(|l| l.contains("[missing]") && l.contains("nonexistent-xyz-binary")),
        "should show [missing] for nonexistent binary, got:\n{}",
        stdout
    );
    assert!(
        stdout.lines().any(|l| l.contains("[found]") && l.contains("echo")),
        "should show [found] for echo, got:\n{}",
        stdout
    );
}

#[test]
fn missing_binary_allows_other_commands() {
    // Arrange — config with one missing binary, but echo is present
    common::assert_user_namespaces();

    let workspace = tempfile::tempdir().expect("create workspace");
    let config = format!(
        r#"bundles:
  test-bundle:
    binaries: [sh, bash, echo, nonexistent-xyz-binary]

profiles:
  test:
    bundles: [test-bundle]
    filesystem:
      workspace: {}
"#,
        workspace.path().to_str().unwrap()
    );
    let mut f = tempfile::NamedTempFile::new().expect("create temp config");
    std::io::Write::write_all(&mut f, config.as_bytes()).expect("write config");

    // Act
    let output = Command::new(common::ostia_bin())
        .args([
            "run", "--config", f.path().to_str().unwrap(),
            "--profile", "test", "--",
            "echo degraded-ok",
        ])
        .output()
        .expect("failed to execute ostia");

    // Assert — command runs despite one binary being unresolvable
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "run should succeed for available commands, stderr={:?}", String::from_utf8_lossy(&output.stderr));
    assert_eq!(stdout.trim(), "degraded-ok");
}
