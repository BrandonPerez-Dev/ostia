//! Built-in bundle definitions.
//!
//! These bundles are available to all configs without explicit definition.
//! Config-defined bundles with the same name take precedence.

use crate::config::Bundle;
use std::collections::HashMap;

pub fn builtin_bundles() -> HashMap<String, Bundle> {
    let mut bundles = HashMap::new();

    bundles.insert(
        "baseline".into(),
        Bundle {
            description: None,
            binaries: vec![
                "sh", "bash", "cat", "grep", "ls", "find", "head", "tail",
                "jq", "wc", "sed", "awk", "echo", "date", "whoami",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            subcommands: vec![],
        },
    );

    bundles.insert(
        "git-read".into(),
        Bundle {
            description: None,
            binaries: vec!["git".into()],
            subcommands: vec![
                "git log *".into(),
                "git diff *".into(),
                "git status".into(),
                "git branch -l".into(),
                "git show *".into(),
                "git rev-parse *".into(),
            ],
        },
    );

    bundles.insert(
        "git-write".into(),
        Bundle {
            description: None,
            binaries: vec!["git".into()],
            subcommands: vec![
                "git add *".into(),
                "git commit *".into(),
                "git push *".into(),
                "git checkout *".into(),
                "git merge *".into(),
                "git rebase *".into(),
                "git stash *".into(),
                "git branch *".into(),
            ],
        },
    );

    bundles.insert(
        "github-read".into(),
        Bundle {
            description: None,
            binaries: vec!["gh".into()],
            subcommands: vec![
                "gh pr list *".into(),
                "gh pr view *".into(),
                "gh issue list *".into(),
                "gh issue view *".into(),
                "gh repo view *".into(),
            ],
        },
    );

    bundles.insert(
        "github-rw".into(),
        Bundle {
            description: None,
            binaries: vec!["gh".into()],
            subcommands: vec![
                "gh pr list *".into(),
                "gh pr view *".into(),
                "gh pr create *".into(),
                "gh issue list *".into(),
                "gh issue view *".into(),
                "gh issue create *".into(),
                "gh repo view *".into(),
            ],
        },
    );

    bundles.insert(
        "k8s-read".into(),
        Bundle {
            description: None,
            binaries: vec!["kubectl".into()],
            subcommands: vec![
                "kubectl get *".into(),
                "kubectl describe *".into(),
                "kubectl logs *".into(),
            ],
        },
    );

    bundles.insert(
        "docker".into(),
        Bundle {
            description: None,
            binaries: vec!["docker".into()],
            subcommands: vec![
                "docker build *".into(),
                "docker run *".into(),
                "docker ps *".into(),
                "docker images *".into(),
                "docker pull *".into(),
            ],
        },
    );

    bundles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_expected_bundles_exist() {
        let bundles = builtin_bundles();
        let expected = ["baseline", "git-read", "git-write", "github-read", "github-rw", "k8s-read", "docker"];
        for name in &expected {
            assert!(bundles.contains_key(*name), "missing built-in bundle: {}", name);
        }
    }

    #[test]
    fn baseline_has_common_utilities() {
        let bundles = builtin_bundles();
        let baseline = &bundles["baseline"];
        for bin in &["echo", "cat", "ls", "grep", "sed"] {
            assert!(
                baseline.binaries.contains(&bin.to_string()),
                "baseline missing: {}",
                bin
            );
        }
    }

    #[test]
    fn git_read_has_no_write_commands() {
        let bundles = builtin_bundles();
        let git_read = &bundles["git-read"];
        for sub in &git_read.subcommands {
            assert!(
                !sub.starts_with("git push") && !sub.starts_with("git commit"),
                "git-read should not allow write commands, found: {}",
                sub
            );
        }
    }
}
