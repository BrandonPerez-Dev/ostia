//! Integration tests: Auth status checking (V5).
//!
//! V5 contracts — validates that auth checks run on the host, surface
//! in `ostia check`, and gate `ostia run` when any service is inactive.

mod common;

use std::process::Command;

/// Contract 1: Auth status display
/// When `ostia check` runs with auth checks configured,
/// Then each service's status (active/inactive) appears on the same line as
/// the service name.
#[test]
fn check_shows_auth_status_per_service() {
    // Arrange
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = format!(
        r#"bundles:
  baseline:
    binaries: [sh, bash, echo, cat, ls]

profiles:
  auth-test:
    bundles: [baseline]
    auth:
      working-service:
        check: "true"
      broken-service:
        check: "false"
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
            "check",
            "--config",
            f.path().to_str().unwrap(),
            "--profile",
            "auth-test",
        ])
        .output()
        .expect("failed to execute ostia");

    // Assert
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "ostia check should succeed, stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout
            .lines()
            .any(|l| l.contains("active") && l.contains("working-service")),
        "should show working-service as active on one line, got:\n{}",
        stdout
    );
    assert!(
        stdout
            .lines()
            .any(|l| l.contains("inactive") && l.contains("broken-service")),
        "should show broken-service as inactive on one line, got:\n{}",
        stdout
    );
}

/// Contract 2: Auth gate blocks inactive auth
/// When `ostia run` is called with an inactive auth check,
/// Then the command never executes, stderr mentions auth + service name,
/// and exit is non-zero.
#[test]
fn run_with_inactive_auth_fails() {
    // Arrange
    common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = format!(
        r#"bundles:
  baseline:
    binaries: [sh, bash, echo]

profiles:
  test:
    bundles: [baseline]
    auth:
      my-service:
        check: "false"
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
            "run",
            "--config",
            f.path().to_str().unwrap(),
            "--profile",
            "test",
            "--",
            "echo",
            "should-not-run",
        ])
        .output()
        .expect("failed to execute ostia");

    // Assert
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "run should fail when auth is inactive"
    );
    assert!(
        stdout.is_empty(),
        "command should not have executed, got stdout: {:?}",
        stdout
    );
    assert!(
        !stdout.contains("should-not-run"),
        "if 'should-not-run' appears, auth gate didn't fire — command ran despite inactive auth"
    );
    assert!(
        stderr.to_lowercase().contains("auth"),
        "error should mention auth, got stderr: {:?}",
        stderr
    );
    assert!(
        stderr.contains("my-service"),
        "error should mention the failing service name, got stderr: {:?}",
        stderr
    );
}

/// Contract 3: Active auth passes through
/// When `ostia run` is called with all auth checks passing,
/// Then the command executes normally.
#[test]
fn run_with_active_auth_succeeds() {
    // Arrange
    common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = format!(
        r#"bundles:
  baseline:
    binaries: [sh, bash, echo]

profiles:
  test:
    bundles: [baseline]
    auth:
      my-service:
        check: "true"
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
            "run",
            "--config",
            f.path().to_str().unwrap(),
            "--profile",
            "test",
            "--",
            "echo",
            "auth-ok",
        ])
        .output()
        .expect("failed to execute ostia");

    // Assert
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "run should succeed with active auth, stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(stdout.trim(), "auth-ok", "command output should pass through");
}

/// Contract 4a: No-auth backward compat — run works
/// When a profile has no auth section,
/// Then `ostia run` executes normally without any auth checks.
#[test]
fn run_without_auth_config_succeeds() {
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
            "no-auth",
        ])
        .output()
        .expect("failed to execute ostia");

    // Assert
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "run should succeed with no auth config, stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(stdout.trim(), "no-auth", "command output should pass through");
}

/// Contract 4b: No-auth backward compat — check shows no auth section
/// When a profile has no auth section,
/// Then `ostia check` does not display an Auth section.
#[test]
fn check_without_auth_shows_no_auth_section() {
    // Arrange
    let config = r#"bundles:
  baseline:
    binaries: [sh, bash, echo]

profiles:
  no-auth:
    bundles: [baseline]
"#;
    let mut f = tempfile::NamedTempFile::new().expect("create temp config");
    std::io::Write::write_all(&mut f, config.as_bytes()).expect("write config");

    // Act
    let output = Command::new(common::ostia_bin())
        .args([
            "check",
            "--config",
            f.path().to_str().unwrap(),
            "--profile",
            "no-auth",
        ])
        .output()
        .expect("failed to execute ostia");

    // Assert
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "ostia check should succeed, stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !stdout.contains("Auth"),
        "check output should NOT have an Auth section when no auth configured, got:\n{}",
        stdout
    );
}
