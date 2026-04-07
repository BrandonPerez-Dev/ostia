//! Integration tests: Seccomp BPF syscall filtering (V3).
//!
//! V3 contracts — validates that seccomp blocks dangerous syscalls
//! (mount, unshare, ptrace) inside the sandbox while allowing normal
//! operations (echo, file I/O, date).

mod common;

use std::process::Command;

/// Contract 1a: Normal commands work under seccomp
/// When normal commands run inside the sandbox,
/// Then seccomp does not block allowed syscalls (write, read, clock_gettime).
#[test]
fn normal_commands_work_with_seccomp() {
    // Arrange
    common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = common::write_sandbox_config(workspace.path().to_str().unwrap(), &["date"]);

    // Act
    let output = Command::new(common::ostia_bin())
        .args([
            "run",
            "--config",
            config.path().to_str().unwrap(),
            "--profile",
            "test",
            "--",
            "echo hello && echo world && date +%Y",
        ])
        .output()
        .expect("failed to execute ostia");

    // Assert
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("hello"),
        "stdout should contain 'hello', got: {:?}",
        stdout
    );
    assert!(
        stdout.contains("world"),
        "stdout should contain 'world', got: {:?}",
        stdout
    );
    assert!(
        stdout.lines().any(|l| l.trim().len() == 4 && l.trim().chars().all(|c| c.is_ascii_digit())),
        "stdout should contain a 4-digit year from date, got: {:?}",
        stdout
    );
    assert!(
        stderr.is_empty(),
        "stderr should be empty for normal commands, got: {:?}",
        stderr
    );
    assert!(
        output.status.success(),
        "normal commands should exit 0, got: {:?}",
        output.status
    );
}

/// Contract 1b: Workspace file I/O works under seccomp
/// When a command writes and reads a file in the workspace,
/// Then seccomp does not block the required syscalls (openat, write, read).
#[test]
fn workspace_io_works_with_seccomp() {
    // Arrange
    common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let ws_path = workspace.path().to_str().unwrap();
    let config = common::write_sandbox_config(ws_path, &[]);

    // Act
    let shell_cmd = format!(
        "echo seccomp-test > {}/file.txt && cat {}/file.txt",
        ws_path, ws_path
    );
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

    // Assert
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stdout.trim(),
        "seccomp-test",
        "should read back what was written"
    );
    assert!(
        stderr.is_empty(),
        "stderr should be empty for workspace I/O, got: {:?}",
        stderr
    );
    assert!(
        output.status.success(),
        "workspace I/O should exit 0, got: {:?}",
        output.status
    );
}

/// Contract 2: Namespace creation blocked
/// When a command attempts to create a new mount namespace via unshare,
/// Then seccomp blocks SYS_unshare with EPERM and the inner command never runs.
#[test]
fn seccomp_blocks_unshare() {
    // Arrange
    common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = common::write_sandbox_config(workspace.path().to_str().unwrap(), &["unshare"]);

    // Act
    let output = Command::new(common::ostia_bin())
        .args([
            "run",
            "--config",
            config.path().to_str().unwrap(),
            "--profile",
            "test",
            "--",
            "unshare --mount echo escaped",
        ])
        .output()
        .expect("failed to execute ostia");

    // Assert
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.is_empty(),
        "stdout should be empty — echo must not execute, got: {:?}",
        stdout
    );
    assert!(
        !stdout.contains("escaped"),
        "if 'escaped' appears in stdout, seccomp failed to block SYS_unshare — sandbox escape"
    );
    assert!(
        stderr.contains("Operation not permitted") || stderr.contains("cannot"),
        "should fail with EPERM, got stderr: {:?}",
        stderr
    );
    assert!(
        !output.status.success(),
        "unshare should be blocked by seccomp"
    );
}

/// Contract 3: Mount blocked
/// When a command attempts to mount a filesystem inside the sandbox,
/// Then seccomp blocks SYS_mount with EPERM.
#[test]
fn seccomp_blocks_mount() {
    // Arrange
    common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let config = common::write_sandbox_config(workspace.path().to_str().unwrap(), &["mount"]);

    // Act
    let output = Command::new(common::ostia_bin())
        .args([
            "run",
            "--config",
            config.path().to_str().unwrap(),
            "--profile",
            "test",
            "--",
            "mount -t tmpfs tmpfs /tmp",
        ])
        .output()
        .expect("failed to execute ostia");

    // Assert
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.is_empty(),
        "stdout should be empty for a blocked mount, got: {:?}",
        stdout
    );
    assert!(
        stderr.contains("Operation not permitted") || stderr.contains("permission denied"),
        "should fail with EPERM, got stderr: {:?}",
        stderr
    );
    assert!(
        !output.status.success(),
        "mount should be blocked by seccomp"
    );
}

/// Contract 4: Process tracing blocked
/// When a command attempts ptrace(PTRACE_TRACEME) inside the sandbox,
/// Then seccomp blocks SYS_ptrace with EPERM.
#[test]
fn seccomp_blocks_ptrace() {
    // Arrange
    common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let ws_path = workspace.path().to_str().unwrap();
    let config = common::write_sandbox_config(ws_path, &[]);

    // Compile a minimal static binary that attempts ptrace(PTRACE_TRACEME).
    // Uses -nostdlib + raw syscalls so it has zero shared library deps —
    // the sandbox doesn't need to mount any libraries for it to run.
    let c_source = r#"
        static long raw_syscall(long nr, long a1, long a2, long a3, long a4) {
            long ret;
            register long r10 __asm__("r10") = a4;
            __asm__ volatile("syscall"
                : "=a"(ret)
                : "a"(nr), "D"(a1), "S"(a2), "d"(a3), "r"(r10)
                : "rcx", "r11", "memory");
            return ret;
        }
        void _start(void) {
            long ret = raw_syscall(101, 0, 0, 0, 0); /* SYS_ptrace, PTRACE_TRACEME */
            if (ret == 0) {
                raw_syscall(1, 1, (long)"ptrace succeeded\n", 17, 0); /* write to stdout */
                raw_syscall(60, 0, 0, 0, 0); /* exit(0) */
            } else {
                raw_syscall(1, 2, (long)"ptrace failed: EPERM\n", 21, 0); /* write to stderr */
                raw_syscall(60, 1, 0, 0, 0); /* exit(1) */
            }
        }
    "#;

    let c_path = workspace.path().join("try-ptrace.c");
    let bin_path = workspace.path().join("try-ptrace");
    std::fs::write(&c_path, c_source).expect("write C source");

    let gcc = Command::new("gcc")
        .args([
            "-nostdlib",
            "-static",
            "-o",
            bin_path.to_str().unwrap(),
            c_path.to_str().unwrap(),
        ])
        .output()
        .expect("gcc must be installed to compile ptrace test fixture");
    assert!(
        gcc.status.success(),
        "failed to compile try-ptrace fixture: {}",
        String::from_utf8_lossy(&gcc.stderr)
    );

    // Act — run via bash -c so the matcher sees "bash" (whitelisted).
    let shell_cmd = format!("{}/try-ptrace", ws_path);
    let output = Command::new(common::ostia_bin())
        .args([
            "run",
            "--config",
            config.path().to_str().unwrap(),
            "--profile",
            "test",
            "--",
        ])
        .arg(&format!("bash -c '{}'", shell_cmd))
        .output()
        .expect("failed to execute ostia");

    // Assert
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains("ptrace succeeded"),
        "if ptrace succeeded, seccomp failed to block SYS_ptrace — sandbox escape"
    );
    assert!(
        stderr.contains("EPERM") || stderr.contains("Operation not permitted"),
        "should fail with EPERM when ptrace is blocked, got stderr: {:?}",
        stderr
    );
    assert!(
        !output.status.success(),
        "ptrace should be blocked by seccomp"
    );
}
