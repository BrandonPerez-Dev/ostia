//! Integration tests: binary-source file provider (Slice 3 of #11).
//! Covers C-BS9 from `spec/binary-source.md`.

mod mcp_common;

use serde_json::json;
use std::io::Write;

// ─── C-BS9 ───

/// C-BS9: File binary source — local path → binary in sandbox. The cache
/// COPY exists separately from the source path; ostia never bind-mounts
/// directly from the source path because the source may not be inside the
/// sandbox's allowed filesystem.
#[test]
fn file_source_lands_in_sandbox() {
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("workspace");
    let cache_dir = mcp_common::temp_binary_cache_dir();

    let bytes = mcp_common::make_fake_binary_bytes("from-file-source");
    let sha = mcp_common::sha256_hex(&bytes);
    let source_dir = tempfile::tempdir().expect("source dir");
    let source_path = source_dir.path().join("myinternal");
    std::fs::write(&source_path, &bytes).expect("write source");

    let config = format!(
        r#"binary_cache_dir: {cache_dir}

binaries:
  myinternal:
    sha256: "{sha}"
    format: binary
    source:
      provider: file
      path: {source_path}

bundles:
  baseline:
    binaries: [sh, bash, myinternal]

profiles:
  test:
    bundles: [baseline]
    filesystem:
      workspace: {workspace}
"#,
        cache_dir = cache_dir.path().display(),
        source_path = source_path.display(),
        workspace = workspace.path().display(),
    );
    let mut config_file = tempfile::NamedTempFile::new().expect("config temp");
    config_file.write_all(config.as_bytes()).expect("write config");

    let mut client = mcp_common::McpClient::spawn(config_file.path());
    client.handshake();

    let response = client.call_tool("test", json!({ "command": "myinternal" }));

    let result = &response["result"];
    let text = mcp_common::get_content_text(result);
    assert!(
        text.contains("from-file-source"),
        "file source binary should run, got: {:?}",
        text
    );

    let cache_path = cache_dir.path().join(&sha).join("myinternal");
    assert!(
        cache_path.exists(),
        "file source should populate the cache at {}",
        cache_path.display()
    );
}
