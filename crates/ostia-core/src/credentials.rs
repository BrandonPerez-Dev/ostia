//! Credential providers: fetch secrets on the host and return flat key-value maps.
//!
//! Follows the External Secrets Operator pattern — provider-agnostic interface
//! where every provider returns `HashMap<String, String>`, and the `inject`
//! block in config maps those keys to sandbox env vars.

use crate::config::{CredentialDef, CredentialEntry};
use std::collections::{BTreeMap, HashMap};

/// Built-in credential presets for common tools.
///
/// Each preset is a `CredentialDef` with a fixed command and inject mapping.
/// Config references them as `gcloud: preset`.
pub fn builtin_presets() -> HashMap<String, CredentialDef> {
    let mut presets = HashMap::new();

    presets.insert("gcloud".to_string(), CredentialDef {
        provider: "command".to_string(),
        command: Some("gcloud auth print-access-token".to_string()),
        env: None,
        path: None,
        url: None,
        headers: HashMap::new(),
        inject: [("CLOUDSDK_AUTH_ACCESS_TOKEN".to_string(), "value".to_string())]
            .into_iter().collect(),
    });

    presets.insert("github".to_string(), CredentialDef {
        provider: "command".to_string(),
        command: Some("gh auth token".to_string()),
        env: None,
        path: None,
        url: None,
        headers: HashMap::new(),
        inject: [("GITHUB_TOKEN".to_string(), "value".to_string())]
            .into_iter().collect(),
    });

    presets.insert("aws".to_string(), CredentialDef {
        provider: "command".to_string(),
        command: Some("aws configure export-credentials --format env-no-export".to_string()),
        env: None,
        path: None,
        url: None,
        headers: HashMap::new(),
        inject: [
            ("AWS_ACCESS_KEY_ID".to_string(), "value".to_string()),
        ].into_iter().collect(),
    });

    presets
}

/// Resolve credential entries: expand presets to full definitions.
pub fn resolve_entries(
    entries: &BTreeMap<String, CredentialEntry>,
) -> anyhow::Result<BTreeMap<String, CredentialDef>> {
    let presets = builtin_presets();
    let mut resolved = BTreeMap::new();

    for (name, entry) in entries {
        let def = match entry {
            CredentialEntry::Custom(def) => def.clone(),
            CredentialEntry::Preset(marker) => {
                if marker != "preset" {
                    anyhow::bail!(
                        "credential '{}': invalid value '{}' (expected 'preset' or a provider definition)",
                        name, marker
                    );
                }
                presets.get(name).cloned().ok_or_else(|| {
                    anyhow::anyhow!("unknown credential preset '{}' (available: gcloud, github, aws)", name)
                })?
            }
        };
        resolved.insert(name.clone(), def);
    }

    Ok(resolved)
}

/// Fetch credentials for all providers in a profile's `credentials:` block.
///
/// `user_id` is an optional identity for template interpolation in HTTP
/// provider URLs and headers (e.g., `{{ user_id }}`).
///
/// Returns the merged env vars to inject into the sandbox. Fails on the
/// first provider error — a failed fetch blocks execution entirely.
pub fn fetch_credentials(
    credentials: &BTreeMap<String, CredentialDef>,
    user_id: Option<&str>,
) -> anyhow::Result<HashMap<String, String>> {
    let mut env = HashMap::new();

    for (name, cred) in credentials {
        let provider_output = fetch_single(name, cred, user_id)?;

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
fn fetch_single(
    name: &str,
    cred: &CredentialDef,
    user_id: Option<&str>,
) -> anyhow::Result<HashMap<String, String>> {
    match cred.provider.as_str() {
        "command" => fetch_command(name, cred),
        "env" => fetch_env(name, cred),
        "file" => fetch_file(name, cred),
        "http" => fetch_http(name, cred, user_id),
        other => anyhow::bail!("unknown credential provider type '{}' for '{}'", other, name),
    }
}

/// Interpolate `{{ user_id }}` in a string. Fails if the template contains
/// `{{ user_id }}` but no user identity is available.
fn interpolate_template(template: &str, user_id: Option<&str>) -> anyhow::Result<String> {
    if !template.contains("{{ user_id }}") {
        return Ok(template.to_string());
    }

    let id = user_id.ok_or_else(|| {
        anyhow::anyhow!("template contains '{{{{ user_id }}}}' but no user identity provided (use --user-id flag or OSTIA_USER_ID env)")
    })?;

    Ok(template.replace("{{ user_id }}", id))
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

/// `env` provider: read a host environment variable.
///
/// Returns `{ "value": env_var_value }`.
fn fetch_env(name: &str, cred: &CredentialDef) -> anyhow::Result<HashMap<String, String>> {
    let var_name = cred
        .env
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("credential '{}': env provider requires 'env' field", name))?;

    let value = std::env::var(var_name).map_err(|_| {
        anyhow::anyhow!("credential '{}': env var '{}' not set", name, var_name)
    })?;

    let mut map = HashMap::new();
    map.insert("value".to_string(), value);
    Ok(map)
}

/// `file` provider: read a host file.
///
/// Returns `{ "value": file_contents }`.
fn fetch_file(name: &str, cred: &CredentialDef) -> anyhow::Result<HashMap<String, String>> {
    let path = cred
        .path
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("credential '{}': file provider requires 'path' field", name))?;

    let contents = std::fs::read_to_string(path).map_err(|e| {
        anyhow::anyhow!("credential '{}': failed to read '{}': {}", name, path, e)
    })?;

    let mut map = HashMap::new();
    map.insert("value".to_string(), contents.trim().to_string());
    Ok(map)
}

/// `http` provider: GET a URL, parse JSON response, return top-level keys.
///
/// Template variables (`{{ user_id }}`) in URL and header values are
/// interpolated before the request.
fn fetch_http(
    name: &str,
    cred: &CredentialDef,
    user_id: Option<&str>,
) -> anyhow::Result<HashMap<String, String>> {
    let url_template = cred
        .url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("credential '{}': http provider requires 'url' field", name))?;

    let url = interpolate_template(url_template, user_id)
        .map_err(|e| anyhow::anyhow!("credential '{}': {}", name, e))?;

    let mut request = ureq::get(&url);
    for (key, value_template) in &cred.headers {
        let value = interpolate_template(value_template, user_id)
            .map_err(|e| anyhow::anyhow!("credential '{}': header '{}': {}", name, key, e))?;
        request = request.header(key, &value);
    }

    let mut response = request
        .call()
        .map_err(|e| anyhow::anyhow!("credential provider '{}' HTTP request failed: {}", name, e))?;

    let status = response.status().as_u16();
    if status >= 400 {
        anyhow::bail!(
            "credential provider '{}' failed (HTTP {})",
            name,
            status
        );
    }

    let body: serde_json::Value = response
        .body_mut()
        .read_json()
        .map_err(|e| anyhow::anyhow!("credential provider '{}': failed to parse JSON response: {}", name, e))?;

    // Flatten top-level JSON keys into string map.
    let map = match body {
        serde_json::Value::Object(obj) => {
            obj.into_iter()
                .filter_map(|(k, v)| {
                    let s = match v {
                        serde_json::Value::String(s) => s,
                        other => other.to_string(),
                    };
                    Some((k, s))
                })
                .collect()
        }
        _ => anyhow::bail!("credential provider '{}': expected JSON object, got {:?}", name, body),
    };

    Ok(map)
}
