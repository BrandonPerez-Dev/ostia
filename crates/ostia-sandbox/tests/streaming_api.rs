//! Integration tests: Streaming execution API (V6.5, Slice 2).
//!
//! Validates that the programmatic streaming API delivers output chunks
//! incrementally, distinguishes stdout from stderr, supports collecting
//! into a buffered ExecutionResult, and delivers chunks before the
//! command exits.

use std::time::{Duration, Instant};

/// Helper: build a SandboxExecutor for streaming tests.
///
/// Returns (executor, _workspace_guard) — hold the guard to keep the
/// tempdir alive for the test's duration.
fn build_test_executor(
    extra_binaries: &[&str],
) -> (ostia_sandbox::SandboxExecutor, tempfile::TempDir) {
    let workspace = tempfile::tempdir().expect("create workspace");
    let ws_path = workspace.path().to_str().unwrap().to_string();

    let mut all_binaries: std::collections::HashSet<String> =
        ["sh", "bash", "echo", "cat", "ls"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    for b in extra_binaries {
        all_binaries.insert(b.to_string());
    }

    let profile = ostia_core::Profile {
        name: "test".to_string(),
        binaries: all_binaries,
        subcommand_allows: vec![],
        subcommand_denies: vec![],
        workspace: Some(ws_path.into()),
        read_paths: vec![],
        deny_read_paths: vec![],
        deny_write_paths: vec![],
        network_allow: vec![],
        auth_checks: vec![],
        env: std::collections::HashMap::new(),
    };

    let executor = ostia_sandbox::SandboxExecutor::from_profile(profile)
        .expect("build executor from profile");
    (executor, workspace)
}

/// Ensure user namespaces are available — hard assert, no silent skip.
fn assert_user_namespaces() {
    let available = std::fs::read_to_string("/proc/sys/kernel/unprivileged_userns_clone")
        .map(|s| s.trim() == "1")
        .unwrap_or(true);
    assert!(
        available,
        "unprivileged user namespaces required"
    );
}

/// Contract 5: Channel receives chunks as they arrive
/// When a command produces multiple lines of output,
/// Then the streaming API delivers 3+ chunks on stdout, each containing
/// the respective line. All chunks arrive. Final result has exit code 0.
#[test]
fn streaming_channel_receives_chunks() {
    assert_user_namespaces();
    let (executor, _ws) = build_test_executor(&[]);

    // Act — use the streaming API
    let rx = executor.execute_streaming("echo one && echo two && echo three")
        .expect("execute_streaming should succeed");

    // Collect all chunks
    let mut stdout_chunks = Vec::new();
    let mut exit_code = None;

    for event in rx {
        match event {
            ostia_sandbox::StreamEvent::Stdout(data) => stdout_chunks.push(data),
            ostia_sandbox::StreamEvent::Stderr(_) => {}
            ostia_sandbox::StreamEvent::Exit(code) => exit_code = Some(code),
        }
    }

    // Assert
    let all_stdout: String = stdout_chunks.concat();
    assert!(
        all_stdout.contains("one"),
        "should receive 'one' in stdout chunks, got: {:?}",
        stdout_chunks
    );
    assert!(
        all_stdout.contains("two"),
        "should receive 'two' in stdout chunks, got: {:?}",
        stdout_chunks
    );
    assert!(
        all_stdout.contains("three"),
        "should receive 'three' in stdout chunks, got: {:?}",
        stdout_chunks
    );
    assert_eq!(
        exit_code,
        Some(0),
        "exit code should be 0"
    );
}

/// Contract 6: Stderr chunks arrive on a separate variant
/// When a command writes to both stdout and stderr,
/// Then the caller can distinguish stdout chunks from stderr chunks
/// via the StreamEvent enum.
#[test]
fn streaming_stderr_chunks_tagged_separately() {
    assert_user_namespaces();
    let (executor, _ws) = build_test_executor(&[]);

    // Act
    let rx = executor.execute_streaming("echo out && bash -c 'echo err >&2'")
        .expect("execute_streaming should succeed");

    // Collect
    let mut stdout_data = String::new();
    let mut stderr_data = String::new();

    for event in rx {
        match event {
            ostia_sandbox::StreamEvent::Stdout(data) => stdout_data.push_str(&data),
            ostia_sandbox::StreamEvent::Stderr(data) => stderr_data.push_str(&data),
            ostia_sandbox::StreamEvent::Exit(_) => {}
        }
    }

    // Assert
    assert!(
        stdout_data.contains("out"),
        "stdout should contain 'out', got: {:?}",
        stdout_data
    );
    assert!(
        stderr_data.contains("err"),
        "stderr should contain 'err', got: {:?}",
        stderr_data
    );
    assert!(
        !stdout_data.contains("err"),
        "stdout must not contain stderr content, got: {:?}",
        stdout_data
    );
    assert!(
        !stderr_data.contains("out"),
        "stderr must not contain stdout content, got: {:?}",
        stderr_data
    );
}

/// Contract 7: ExecutionResult convenience wrapper
/// When the caller collects a stream into an ExecutionResult,
/// Then the result matches the existing buffered API shape.
#[test]
fn streaming_collect_into_execution_result() {
    assert_user_namespaces();
    let (executor, _ws) = build_test_executor(&[]);

    // Act — use convenience method that wraps streaming into ExecutionResult
    let result = executor.execute_streaming_collect("echo collected")
        .expect("execute_streaming_collect should succeed");

    // Assert — same shape as existing execute()
    assert!(result.allowed, "command should be allowed");
    assert_eq!(result.exit_code, 0, "exit code should be 0");
    assert_eq!(
        result.stdout.trim(),
        "collected",
        "stdout should match command output"
    );
    assert!(
        result.stderr.is_empty(),
        "stderr should be empty, got: {:?}",
        result.stderr
    );
}

/// Contract 8: Long-running command streams before exit
/// When a command produces output over time,
/// Then the first chunk arrives before the command finishes.
/// This is the timing-sensitive test that proves streaming works.
#[test]
fn streaming_first_chunk_arrives_before_exit() {
    assert_user_namespaces();
    let (executor, _ws) = build_test_executor(&["sleep"]);

    let start = Instant::now();

    // Act — command takes ~0.9s total, first output is immediate
    let rx = executor.execute_streaming(
        "echo first && sleep 0.3 && echo second && sleep 0.3 && echo third"
    ).expect("execute_streaming should succeed");

    // Wait for just the first stdout chunk
    let mut first_chunk_time = None;
    let mut last_event_time = None;

    for event in rx {
        match event {
            ostia_sandbox::StreamEvent::Stdout(_) if first_chunk_time.is_none() => {
                first_chunk_time = Some(start.elapsed());
            }
            ostia_sandbox::StreamEvent::Exit(_) => {
                last_event_time = Some(start.elapsed());
            }
            _ => {}
        }
    }

    // Assert — first chunk should arrive well before the command exits
    let first = first_chunk_time.expect("should have received at least one stdout chunk");
    let last = last_event_time.expect("should have received exit event");

    assert!(
        first < Duration::from_millis(500),
        "first chunk should arrive within 500ms, took: {:?}",
        first
    );
    assert!(
        last > Duration::from_millis(400),
        "command should take at least 400ms total (has sleeps), took: {:?}",
        last
    );
    assert!(
        first < last - Duration::from_millis(200),
        "first chunk ({:?}) should arrive well before exit ({:?}) — proves streaming, not buffering",
        first,
        last
    );
}
