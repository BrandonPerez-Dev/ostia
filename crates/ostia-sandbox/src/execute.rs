use crate::namespace;
use crate::resolve::{self, ResolvedBinary};
use ostia_core::{CommandMatcher, OstiaConfig, Profile};

use anyhow::{Context, Result};
use nix::sys::wait::{WaitStatus, waitpid};
use nix::unistd::{ForkResult, Pid, close, dup2, execvp, fork, pipe, read};
use std::collections::HashMap;
use std::ffi::CString;
use std::os::fd::{IntoRawFd, RawFd};
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::thread;

/// Result of a sandboxed command execution.
pub struct ExecutionResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub command: String,
    pub allowed: bool,
    pub reason: Option<String>,
}

/// Events emitted during streaming execution.
pub enum StreamEvent {
    /// A chunk of stdout data.
    Stdout(String),
    /// A chunk of stderr data.
    Stderr(String),
    /// The child process exited with this code.
    Exit(i32),
}

/// Executes commands inside a sandboxed environment.
///
/// Ties together config loading, command matching, dependency resolution,
/// and namespace-based isolation to run shell commands under a restricted
/// profile.
pub struct SandboxExecutor {
    profile: Profile,
    matcher: CommandMatcher,
    resolved_binaries: HashMap<String, ResolvedBinary>,
}

impl SandboxExecutor {
    /// Construct a new executor by loading config, resolving the profile,
    /// building the command matcher, and resolving all binary dependencies.
    ///
    /// Binaries that cannot be found on the host produce a warning but do
    /// not cause the constructor to fail.
    pub fn new(config_path: &Path, profile_name: &str) -> Result<Self> {
        let config = OstiaConfig::load(config_path)
            .with_context(|| format!("failed to load config from {}", config_path.display()))?;

        let profile = config
            .resolve_profile(profile_name)
            .with_context(|| format!("failed to resolve profile '{}'", profile_name))?;

        let matcher = CommandMatcher::new(
            profile.binaries.clone(),
            &profile.subcommand_allows,
            &profile.subcommand_denies,
        )
        .context("failed to build command matcher")?;

        let resolution_results = resolve::resolve_profile_binaries(&profile.binaries);

        let mut resolved_binaries = HashMap::new();
        for (name, result) in resolution_results {
            match result {
                Ok(resolved) => {
                    resolved_binaries.insert(name, resolved);
                }
                Err(e) => {
                    eprintln!("warning: could not resolve binary '{}': {}", name, e);
                }
            }
        }

        Ok(Self {
            profile,
            matcher,
            resolved_binaries,
        })
    }

    /// Construct from an already-resolved profile.
    pub fn from_profile(profile: Profile) -> Result<Self> {
        let matcher = CommandMatcher::new(
            profile.binaries.clone(),
            &profile.subcommand_allows,
            &profile.subcommand_denies,
        )
        .context("failed to build command matcher")?;

        let resolution_results = resolve::resolve_profile_binaries(&profile.binaries);

        let mut resolved_binaries = HashMap::new();
        for (name, result) in resolution_results {
            match result {
                Ok(resolved) => {
                    resolved_binaries.insert(name, resolved);
                }
                Err(e) => {
                    eprintln!("warning: could not resolve binary '{}': {}", name, e);
                }
            }
        }

        Ok(Self {
            profile,
            matcher,
            resolved_binaries,
        })
    }

    /// Fork a child process that sets up the sandbox (namespace, landlock,
    /// seccomp) and execs `/bin/sh -c "<command>"`.
    ///
    /// Returns `(child_pid, stdout_read_fd, stderr_read_fd)`. The caller
    /// is responsible for reading from those fds, closing them, and calling
    /// `waitpid` on the child.
    ///
    /// # Safety
    ///
    /// Uses `fork()`. The child immediately sets up file descriptors,
    /// enters a namespace, and execs. No async-signal-unsafe work is done
    /// between fork and exec beyond the namespace setup (which uses only
    /// direct syscalls via nix).
    fn fork_sandboxed(&self, command: &str) -> Result<(Pid, RawFd, RawFd)> {
        // Set up pipes for stdout and stderr capture.
        //
        // nix 0.29 pipe() returns (OwnedFd, OwnedFd). Convert to raw fds
        // so we can manipulate them across the fork boundary without the
        // OwnedFd drop-close semantics interfering with the child.
        let (stdout_read_owned, stdout_write_owned) =
            pipe().context("failed to create stdout pipe")?;
        let (stderr_read_owned, stderr_write_owned) =
            pipe().context("failed to create stderr pipe")?;

        let stdout_read: RawFd = stdout_read_owned.into_raw_fd();
        let stdout_write: RawFd = stdout_write_owned.into_raw_fd();
        let stderr_read: RawFd = stderr_read_owned.into_raw_fd();
        let stderr_write: RawFd = stderr_write_owned.into_raw_fd();

        let fork_result = unsafe { fork() }.context("fork failed")?;

        match fork_result {
            ForkResult::Child => {
                // --- child process ---
                // Close the read ends; the parent owns those.
                let _ = close(stdout_read);
                let _ = close(stderr_read);

                // Redirect stdout and stderr to the write ends of the pipes.
                if dup2(stdout_write, libc::STDOUT_FILENO).is_err() {
                    std::process::exit(126);
                }
                if dup2(stderr_write, libc::STDERR_FILENO).is_err() {
                    std::process::exit(126);
                }

                // Close the now-duplicated originals.
                let _ = close(stdout_write);
                let _ = close(stderr_write);

                // Build a contiguous &[ResolvedBinary] for the namespace call.
                // We use ptr::read to shallow-copy values out of the HashMap
                // without requiring Clone. This is sound because we are in
                // the child post-fork and will either exec or _exit; the
                // parent's memory is COW-protected and no destructors run
                // on the originals in this process.
                let binaries_vec: Vec<ResolvedBinary> = unsafe {
                    self.resolved_binaries
                        .values()
                        .map(|b| std::ptr::read(b))
                        .collect()
                };

                if let Err(e) = namespace::setup_sandbox_namespace(
                    &binaries_vec,
                    self.profile.workspace.as_deref(),
                    &self.profile.read_paths,
                ) {
                    eprintln!("ostia: namespace setup failed: {}", e);
                    std::process::exit(125);
                }

                // Prevent destructors from running on the ptr::read copies.
                std::mem::forget(binaries_vec);

                // Apply Landlock filesystem restrictions (defense-in-depth).
                // This constrains writes to the workspace only, even though the
                // tmpfs root is technically writable after pivot_root.
                if let Err(e) = crate::landlock::apply_landlock_restrictions(
                    self.profile.workspace.as_deref(),
                    &self.profile.read_paths,
                ) {
                    eprintln!("ostia: landlock setup failed: {}", e);
                    std::process::exit(125);
                }

                // Apply seccomp BPF filter (innermost security layer).
                // Blocks mount, unshare, ptrace, and other escape syscalls.
                if let Err(e) = crate::seccomp::apply_seccomp_filter() {
                    eprintln!("ostia: seccomp setup failed: {}", e);
                    std::process::exit(125);
                }

                // Exec /bin/sh -c "<command>".
                let sh = CString::new("/bin/sh").unwrap();
                let dash_c = CString::new("-c").unwrap();
                let cmd = CString::new(command).unwrap_or_else(|_| {
                    // The command contained an interior NUL; abort.
                    std::process::exit(124);
                    #[allow(unreachable_code)]
                    CString::new("").unwrap()
                });

                // execvp never returns on success.
                let _err = execvp(&sh, &[&sh, &dash_c, &cmd]);
                // If we get here, exec failed.
                std::process::exit(127);
            }

            ForkResult::Parent { child } => {
                // --- parent process ---
                // Close the write ends; the child owns those.
                close(stdout_write).ok();
                close(stderr_write).ok();

                Ok((child, stdout_read, stderr_read))
            }
        }
    }

    /// Execute a command inside the sandbox.
    ///
    /// The command is first checked against the profile's allow/deny rules.
    /// If denied, an `ExecutionResult` with `allowed = false` is returned
    /// immediately without forking.
    ///
    /// If allowed, the executor forks a child process that sets up a sandbox
    /// namespace (mount, PID, etc.) and then execs `/bin/sh -c "<command>"`.
    /// The parent captures stdout and stderr through pipes and waits for the
    /// child to exit.
    pub fn execute(&self, command: &str) -> Result<ExecutionResult> {
        // Step 1: check if the command is permitted by the profile.
        if let Err(reason) = self.matcher.check(command) {
            return Ok(ExecutionResult {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: -1,
                command: command.to_string(),
                allowed: false,
                reason: Some(reason),
            });
        }

        // Step 1b: check auth status — fail before forking if any service is inactive.
        if !self.profile.auth_checks.is_empty() {
            let results = ostia_core::run_auth_checks(&self.profile.auth_checks);
            let failed: Vec<_> = results.iter().filter(|r| !r.active).collect();
            if !failed.is_empty() {
                let names: Vec<_> = failed.iter().map(|r| r.service.as_str()).collect();
                return Ok(ExecutionResult {
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: -1,
                    command: command.to_string(),
                    allowed: false,
                    reason: Some(format!("auth required: {} inactive", names.join(", "))),
                });
            }
        }

        // Step 2: fork the sandboxed child.
        let (child, stdout_read, stderr_read) = self.fork_sandboxed(command)?;

        // Step 3: read all output from the child.
        let stdout = read_fd_to_string(stdout_read);
        let stderr = read_fd_to_string(stderr_read);

        close(stdout_read).ok();
        close(stderr_read).ok();

        // Step 4: wait for the child to exit.
        let exit_code = match waitpid(child, None)
            .context("failed to wait for child process")?
        {
            WaitStatus::Exited(_pid, code) => code,
            WaitStatus::Signaled(_pid, signal, _core_dump) => 128 + signal as i32,
            other => {
                anyhow::bail!("unexpected wait status: {:?}", other);
            }
        };

        Ok(ExecutionResult {
            stdout,
            stderr,
            exit_code,
            command: command.to_string(),
            allowed: true,
            reason: None,
        })
    }

    /// Execute a command inside the sandbox with streaming output.
    ///
    /// Returns a channel receiver that yields `StreamEvent` chunks as they
    /// arrive. Stdout and stderr are delivered as separate variants. The
    /// final event is always `StreamEvent::Exit(code)`.
    ///
    /// The child process runs in the background — the caller iterates the
    /// receiver to consume events.
    pub fn execute_streaming(
        &self,
        command: &str,
    ) -> Result<Receiver<StreamEvent>> {
        if let Err(_reason) = self.matcher.check(command) {
            let (tx, rx) = mpsc::channel();
            let _ = tx.send(StreamEvent::Exit(-1));
            return Ok(rx);
        }

        if !self.profile.auth_checks.is_empty() {
            let results = ostia_core::run_auth_checks(&self.profile.auth_checks);
            let failed: Vec<_> = results.iter().filter(|r| !r.active).collect();
            if !failed.is_empty() {
                let (tx, rx) = mpsc::channel();
                let _ = tx.send(StreamEvent::Exit(-1));
                return Ok(rx);
            }
        }

        let (child, stdout_read, stderr_read) = self.fork_sandboxed(command)?;
        let (tx, rx) = mpsc::channel();

        let tx_stdout = tx.clone();
        let tx_stderr = tx.clone();

        let stdout_handle = thread::spawn(move || {
            stream_fd(stdout_read, tx_stdout, StreamEvent::Stdout);
            close(stdout_read).ok();
        });

        let stderr_handle = thread::spawn(move || {
            stream_fd(stderr_read, tx_stderr, StreamEvent::Stderr);
            close(stderr_read).ok();
        });

        // Waiter thread: joins both readers, then waitpids the child and
        // sends the final Exit event. Dropping tx closes the channel.
        thread::spawn(move || {
            stdout_handle.join().ok();
            stderr_handle.join().ok();

            let exit_code = match waitpid(child, None) {
                Ok(WaitStatus::Exited(_, code)) => code,
                Ok(WaitStatus::Signaled(_, signal, _)) => 128 + signal as i32,
                _ => -1,
            };

            let _ = tx.send(StreamEvent::Exit(exit_code));
        });

        Ok(rx)
    }

    /// Execute a command and collect the streaming output into an
    /// `ExecutionResult`, matching the shape of `execute()`.
    pub fn execute_streaming_collect(&self, command: &str) -> Result<ExecutionResult> {
        let rx = self.execute_streaming(command)?;

        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut exit_code = -1;

        for event in rx {
            match event {
                StreamEvent::Stdout(data) => stdout.push_str(&data),
                StreamEvent::Stderr(data) => stderr.push_str(&data),
                StreamEvent::Exit(code) => exit_code = code,
            }
        }

        Ok(ExecutionResult {
            stdout,
            stderr,
            exit_code,
            command: command.to_string(),
            allowed: true,
            reason: None,
        })
    }
}

/// Read all bytes from a raw file descriptor and return them as a `String`.
///
/// Invalid UTF-8 sequences are replaced with the Unicode replacement
/// character. The fd is *not* closed by this function.
fn read_fd_to_string(fd: RawFd) -> String {
    let mut buf = [0u8; 8192];
    let mut output = Vec::new();

    loop {
        match read(fd, &mut buf) {
            Ok(0) => break,
            Ok(n) => output.extend_from_slice(&buf[..n]),
            Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => break,
        }
    }

    String::from_utf8_lossy(&output).into_owned()
}

/// Read chunks from a raw fd and send each as a `StreamEvent` via the
/// channel. Stops when EOF is reached or the receiver is dropped.
fn stream_fd(fd: RawFd, tx: mpsc::Sender<StreamEvent>, wrap: fn(String) -> StreamEvent) {
    let mut buf = [0u8; 8192];

    loop {
        match read(fd, &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let data = String::from_utf8_lossy(&buf[..n]).into_owned();
                if tx.send(wrap(data)).is_err() {
                    break;
                }
            }
            Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denied_command_returns_immediately() {
        // Build a matcher that only allows "echo".
        let mut binaries = std::collections::HashSet::new();
        binaries.insert("echo".to_string());

        let matcher = CommandMatcher::new(binaries.clone(), &[], &[]).unwrap();

        let executor = SandboxExecutor {
            profile: Profile {
                name: "test".to_string(),
                binaries,
                subcommand_allows: vec![],
                subcommand_denies: vec![],
                workspace: None,
                read_paths: vec![],
                deny_read_paths: vec![],
                deny_write_paths: vec![],
                network_allow: vec![],
                auth_checks: vec![],
            },
            matcher,
            resolved_binaries: HashMap::new(),
        };

        let result = executor.execute("curl http://evil.com").unwrap();
        assert!(!result.allowed);
        assert!(result.reason.is_some());
        assert!(result.reason.unwrap().contains("not whitelisted"));
    }
}
