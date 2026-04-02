use glob::Pattern;

/// POSIX shell builtins. Implicitly available when sh or bash is in the
/// allowed binaries, since all commands execute via `/bin/sh -c`.
const SHELL_BUILTINS: &[&str] = &[
    // Special builtins
    "break", "continue", "eval", "exec", "exit", "export", "readonly",
    "return", "set", "shift", "times", "trap", "unset",
    // Regular builtins
    "alias", "bg", "cd", "command", "false", "fg", "getopts", "hash",
    "jobs", "kill", "pwd", "read", "true", "type", "ulimit", "umask",
    "unalias", "wait",
    // Commonly used builtins
    "builtin", "declare", "local", "let", "printf", "source", "test",
];

/// Validates commands against a profile's subcommand allow/deny patterns.
pub struct CommandMatcher {
    allows: Vec<Pattern>,
    denies: Vec<Pattern>,
    allowed_binaries: std::collections::HashSet<String>,
}

impl CommandMatcher {
    pub fn new(
        mut allowed_binaries: std::collections::HashSet<String>,
        allow_patterns: &[String],
        deny_patterns: &[String],
    ) -> anyhow::Result<Self> {
        // When sh or bash is allowed, shell builtins are implicitly available.
        if allowed_binaries.contains("sh") || allowed_binaries.contains("bash") {
            for builtin in SHELL_BUILTINS {
                allowed_binaries.insert((*builtin).to_string());
            }
        }

        let allows = allow_patterns
            .iter()
            .map(|p| Pattern::new(p))
            .collect::<Result<Vec<_>, _>>()?;
        let denies = deny_patterns
            .iter()
            .map(|p| Pattern::new(p))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            allows,
            denies,
            allowed_binaries,
        })
    }

    /// Check if a command is allowed. Returns Err with reason if denied.
    pub fn check(&self, command: &str) -> Result<(), String> {
        // Split compound commands
        let subcommands = split_compound_command(command);

        for subcmd in &subcommands {
            let subcmd = subcmd.trim();
            if subcmd.is_empty() {
                continue;
            }
            self.check_single(subcmd)?;
        }
        Ok(())
    }

    fn check_single(&self, command: &str) -> Result<(), String> {
        // Extract binary name (first token)
        let binary = command
            .split_whitespace()
            .next()
            .ok_or_else(|| "empty command".to_string())?;

        // Strip path prefix to get binary name
        let binary_name = std::path::Path::new(binary)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(binary);

        // Check binary is whitelisted
        if !self.allowed_binaries.contains(binary_name) {
            return Err(format!(
                "binary '{}' is not whitelisted in this profile",
                binary_name
            ));
        }

        // Check deny patterns first (deny overrides allow)
        for pattern in &self.denies {
            if pattern.matches(command) {
                return Err(format!(
                    "subcommand '{}' is denied by pattern '{}'",
                    command, pattern
                ));
            }
        }

        // If there are subcommand allow patterns, command must match at least one
        if !self.allows.is_empty() {
            // Check if any allow pattern matches
            let binary_has_allows = self.allows.iter().any(|p| {
                p.as_str().starts_with(binary_name)
            });

            // Only enforce subcommand matching if this binary has specific allow patterns
            if binary_has_allows {
                let matched = self.allows.iter().any(|p| p.matches(command));
                if !matched {
                    return Err(format!(
                        "subcommand '{}' is not allowed in this profile",
                        command
                    ));
                }
            }
        }

        Ok(())
    }
}

/// Split a shell command on compound operators (&&, ||, ;, |)
/// while respecting single and double quotes.
fn split_compound_command(command: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double_quote => {
                in_single_quote = !in_single_quote;
                current.push(c);
            }
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
                current.push(c);
            }
            '&' if !in_single_quote && !in_double_quote => {
                if chars.peek() == Some(&'&') {
                    chars.next();
                    parts.push(current.clone());
                    current.clear();
                } else {
                    current.push(c);
                }
            }
            '|' if !in_single_quote && !in_double_quote => {
                if chars.peek() == Some(&'|') {
                    chars.next();
                    parts.push(current.clone());
                    current.clear();
                } else {
                    // Pipe — also a boundary
                    parts.push(current.clone());
                    current.clear();
                }
            }
            ';' if !in_single_quote && !in_double_quote => {
                parts.push(current.clone());
                current.clear();
            }
            _ => {
                current.push(c);
            }
        }
    }

    if !current.trim().is_empty() {
        parts.push(current);
    }

    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_matcher(
        binaries: &[&str],
        allows: &[&str],
        denies: &[&str],
    ) -> CommandMatcher {
        CommandMatcher::new(
            binaries.iter().map(|s| s.to_string()).collect(),
            &allows.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            &denies.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
        .unwrap()
    }

    #[test]
    fn test_allowed_binary_no_subcommand_rules() {
        let m = make_matcher(&["echo", "ls"], &[], &[]);
        assert!(m.check("echo hello").is_ok());
        assert!(m.check("ls -la").is_ok());
    }

    #[test]
    fn test_disallowed_binary() {
        let m = make_matcher(&["echo"], &[], &[]);
        let result = m.check("curl http://evil.com");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not whitelisted"));
    }

    #[test]
    fn test_subcommand_allow() {
        let m = make_matcher(
            &["gh"],
            &["gh pr list *", "gh pr view *"],
            &[],
        );
        assert!(m.check("gh pr list --repo foo").is_ok());
        assert!(m.check("gh pr view 42").is_ok());
        assert!(m.check("gh pr merge 42").is_err());
    }

    #[test]
    fn test_deny_overrides_allow() {
        let m = make_matcher(
            &["git"],
            &["git *"],
            &["git push *"],
        );
        assert!(m.check("git status").is_ok());
        assert!(m.check("git push origin main").is_err());
    }

    #[test]
    fn test_compound_command_all_allowed() {
        let m = make_matcher(&["echo", "ls"], &[], &[]);
        assert!(m.check("echo hello && ls -la").is_ok());
    }

    #[test]
    fn test_compound_command_one_blocked() {
        let m = make_matcher(&["echo"], &[], &[]);
        let result = m.check("echo hello && curl evil.com");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("curl"));
    }

    #[test]
    fn test_pipe_command_blocked() {
        let m = make_matcher(&["gh"], &["gh pr list *"], &[]);
        let result = m.check("gh pr list | xargs rm");
        assert!(result.is_err());
    }

    #[test]
    fn test_split_respects_quotes() {
        let parts = split_compound_command("echo 'hello && world' && ls");
        assert_eq!(parts.len(), 2);
        assert!(parts[0].contains("hello && world"));
    }
}
