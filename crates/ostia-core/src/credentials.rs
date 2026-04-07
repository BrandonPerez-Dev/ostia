//! Credential providers: fetch secrets on the host and return flat key-value maps.
//!
//! Follows the External Secrets Operator pattern — provider-agnostic interface
//! where every provider returns `HashMap<String, String>`, and the `inject`
//! block in config maps those keys to sandbox env vars.

use crate::config::CredentialDef;
use std::collections::HashMap;

/// Fetch credentials for all providers in a profile's `credentials:` block.
///
/// Returns the merged env vars to inject into the sandbox. Fails on the
/// first provider error — a failed fetch blocks execution entirely.
pub fn fetch_credentials(
    credentials: &std::collections::BTreeMap<String, CredentialDef>,
) -> anyhow::Result<HashMap<String, String>> {
    let mut env = HashMap::new();

    for (name, cred) in credentials {
        let provider_output = fetch_single(name, cred)?;

        // Map provider output keys → sandbox env var names via the inject block.
        for (env_var, output_key) in &cred.inject {
            if let Some(value) = provider_output.get(output_key) {
                env.insert(env_var.clone(), value.clone());
            }
        }
    }

    Ok(env)
}

/// Fetch from a single credential provider.
///
/// Returns the provider's output as a flat key-value map.
fn fetch_single(name: &str, cred: &CredentialDef) -> anyhow::Result<HashMap<String, String>> {
    match cred.provider.as_str() {
        "command" => fetch_command(name, cred),
        other => anyhow::bail!("unknown credential provider type '{}' for '{}'", other, name),
    }
}

/// `command` provider: shell out on the host, capture stdout.
///
/// Returns `{ "value": trimmed_stdout }`.
fn fetch_command(name: &str, cred: &CredentialDef) -> anyhow::Result<HashMap<String, String>> {
    let command = cred
        .command
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("credential '{}': command provider requires 'command' field", name))?;

    let output = std::process::Command::new("/bin/sh")
        .args(["-c", command])
        .output()
        .map_err(|e| anyhow::anyhow!("credential provider '{}' failed to execute: {}", name, e))?;

    if !output.status.success() {
        anyhow::bail!(
            "credential provider '{}' failed (exit {})",
            name,
            output.status.code().unwrap_or(-1)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let mut map = HashMap::new();
    map.insert("value".to_string(), stdout);
    Ok(map)
}
