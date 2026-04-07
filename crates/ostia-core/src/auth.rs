//! Auth status checking.
//!
//! Runs auth check commands on the host (outside the sandbox) and reports
//! whether each configured service is active or inactive. Commands that
//! exit 0 are considered active; anything else is inactive.

use crate::config::AuthCheck;
use std::process::Command;

/// Result of a single auth check.
#[derive(Debug, Clone)]
pub struct AuthResult {
    pub service: String,
    pub active: bool,
    pub message: String,
}

/// Run all auth checks and return their results.
///
/// Each check runs `/bin/sh -c "<command>"` on the host. Exit code 0
/// means active; any other exit code means inactive.
pub fn run_auth_checks(checks: &[AuthCheck]) -> Vec<AuthResult> {
    checks.iter().map(run_single_check).collect()
}

fn run_single_check(check: &AuthCheck) -> AuthResult {
    let output = Command::new("/bin/sh")
        .args(["-c", &check.command])
        .output();

    match output {
        Ok(out) => {
            let active = out.status.success();
            let msg = if active {
                String::from("active")
            } else {
                let stderr = String::from_utf8_lossy(&out.stderr);
                let trimmed = stderr.trim();
                if trimmed.is_empty() {
                    format!("inactive (exit {})", out.status.code().unwrap_or(-1))
                } else {
                    format!("inactive: {}", trimmed.lines().next().unwrap_or(""))
                }
            };
            AuthResult {
                service: check.service.clone(),
                active,
                message: msg,
            }
        }
        Err(e) => AuthResult {
            service: check.service.clone(),
            active: false,
            message: format!("check failed: {}", e),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_zero_is_active() {
        let checks = vec![AuthCheck {
            service: "test-svc".to_string(),
            command: "true".to_string(),
        }];
        let results = run_auth_checks(&checks);
        assert_eq!(results.len(), 1);
        assert!(results[0].active);
        assert_eq!(results[0].message, "active");
    }

    #[test]
    fn exit_nonzero_is_inactive() {
        let checks = vec![AuthCheck {
            service: "test-svc".to_string(),
            command: "false".to_string(),
        }];
        let results = run_auth_checks(&checks);
        assert_eq!(results.len(), 1);
        assert!(!results[0].active);
        assert!(results[0].message.contains("inactive"));
    }

    #[test]
    fn multiple_checks() {
        let checks = vec![
            AuthCheck {
                service: "good".to_string(),
                command: "true".to_string(),
            },
            AuthCheck {
                service: "bad".to_string(),
                command: "false".to_string(),
            },
        ];
        let results = run_auth_checks(&checks);
        assert_eq!(results.len(), 2);
        assert!(results[0].active);
        assert!(!results[1].active);
    }
}
