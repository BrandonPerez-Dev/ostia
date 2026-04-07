/// Integration tests for per-profile MCP tools (V10, Slice 1).
///
/// Tests that tools/list returns one tool per config profile with curated
/// descriptions, replacing the static run_command/list_commands interface.

mod mcp_common;

use serde_json::json;

/// Contract 31: tools/list returns per-profile tools with correct descriptions
///
/// Setup: Config with 2 profiles. `baseline` bundle (no description). `dev-tools`
/// bundle with `description: "git, curl, jq"`. Profile `test` (description:
/// "Test development sandbox", bundles: [baseline, dev-tools], workspace set).
/// Profile `filtered` (bundles: [baseline, dev-tools], deny: ["rm *", "fakecmd *"]
/// where rm is in baseline, fakecmd is not).
///
/// Expected:
/// - Exactly 2 tools
/// - Tool names: `test` and `filtered`
/// - No `run_command` or `list_commands`
/// - `test` tool description contains: "Test development sandbox", "git", "curl",
///   "jq", workspace path
/// - `test` tool inputSchema has `command` property, does NOT have `profile` property
/// - `filtered` tool description contains "rm" (notable denial)
/// - `filtered` tool description does NOT contain "fakecmd" (non-notable denial)
#[test]
fn mcp_profile_tools_list_returns_per_profile_tools() {
    // Arrange — config with described bundles and two profiles
    mcp_common::assert_user_namespaces();
    let workspace = tempfile::tempdir().expect("create workspace");
    let ws_path = workspace.path().to_str().unwrap();

    // Need a config with both profiles: "test" (described) and "filtered" (deny filter)
    // Combine both into one config inline since existing helpers create them separately
    let config = write_c31_config(ws_path);
    let mut client = mcp_common::McpClient::spawn(config.path());

    // Act — handshake then tools/list
    client.handshake();
    let response = client.tools_list();

    // Assert — exactly 2 tools
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools should be an array");
    assert_eq!(
        tools.len(),
        2,
        "should have exactly 2 tools (one per profile), got: {:?}",
        tools.iter().filter_map(|t| t["name"].as_str()).collect::<Vec<_>>()
    );

    // Assert — tool names are profile names
    let tool_names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert!(
        tool_names.contains(&"test"),
        "should have a tool named 'test', got: {:?}",
        tool_names
    );
    assert!(
        tool_names.contains(&"filtered"),
        "should have a tool named 'filtered', got: {:?}",
        tool_names
    );

    // Assert — no legacy tools
    assert!(
        !tool_names.contains(&"run_command"),
        "should NOT have run_command tool, got: {:?}",
        tool_names
    );
    assert!(
        !tool_names.contains(&"list_commands"),
        "should NOT have list_commands tool, got: {:?}",
        tool_names
    );

    // Assert — "test" tool description content
    let test_tool = tools
        .iter()
        .find(|t| t["name"].as_str() == Some("test"))
        .expect("test tool should exist");
    let test_desc = test_tool["description"]
        .as_str()
        .expect("test tool should have a description");

    assert!(
        test_desc.contains("Test development sandbox"),
        "test tool description should contain profile description, got: {:?}",
        test_desc
    );
    assert!(
        test_desc.contains("git"),
        "test tool description should contain featured bundle tool 'git', got: {:?}",
        test_desc
    );
    assert!(
        test_desc.contains("curl"),
        "test tool description should contain featured bundle tool 'curl', got: {:?}",
        test_desc
    );
    assert!(
        test_desc.contains("jq"),
        "test tool description should contain featured bundle tool 'jq', got: {:?}",
        test_desc
    );
    assert!(
        test_desc.contains(ws_path),
        "test tool description should contain workspace path '{}', got: {:?}",
        ws_path,
        test_desc
    );

    // Assert — "test" tool inputSchema has command, not profile
    let test_schema = &test_tool["inputSchema"];
    assert!(
        test_schema["properties"]["command"].is_object(),
        "test tool schema should have 'command' property, got: {:?}",
        test_schema
    );
    assert!(
        test_schema["properties"]["profile"].is_null(),
        "test tool schema should NOT have 'profile' property, got: {:?}",
        test_schema
    );

    // Assert — "filtered" tool description contains notable denial
    let filtered_tool = tools
        .iter()
        .find(|t| t["name"].as_str() == Some("filtered"))
        .expect("filtered tool should exist");
    let filtered_desc = filtered_tool["description"]
        .as_str()
        .expect("filtered tool should have a description");

    assert!(
        filtered_desc.contains("rm"),
        "filtered description should mention 'rm' (notable denial — binary is in profile), got: {:?}",
        filtered_desc
    );
    assert!(
        !filtered_desc.contains("fakecmd"),
        "filtered description should NOT mention 'fakecmd' (non-notable — binary not in profile), got: {:?}",
        filtered_desc
    );
}

/// Contract 31 (error case): Config with zero profiles returns empty tools array
#[test]
fn mcp_profile_tools_list_empty_when_no_profiles() {
    // Arrange — config with no profiles
    mcp_common::assert_user_namespaces();
    let config = write_empty_profiles_config();
    let mut client = mcp_common::McpClient::spawn(config.path());

    // Act
    client.handshake();
    let response = client.tools_list();

    // Assert — empty tools array, no legacy tools
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools should be an array");
    assert!(
        tools.is_empty(),
        "should have zero tools with no profiles configured, got: {:?}",
        tools.iter().filter_map(|t| t["name"].as_str()).collect::<Vec<_>>()
    );
}

// ─── Config writers specific to C31 ───

/// Config combining both profiles needed for C31:
/// - "test": described profile with featured bundle
/// - "filtered": profile with notable and non-notable denials
fn write_c31_config(workspace: &str) -> tempfile::NamedTempFile {
    let config = format!(
        r#"bundles:
  baseline:
    binaries: [sh, bash, echo, cat, ls, rm]
  dev-tools:
    description: "git, curl, jq"
    binaries: [git, curl, jq]

profiles:
  test:
    description: "Test development sandbox"
    bundles: [baseline, dev-tools]
    filesystem:
      workspace: {workspace}
  filtered:
    description: "Deny filter test"
    bundles: [baseline, dev-tools]
    deny:
      - "rm *"
      - "fakecmd *"
    filesystem:
      workspace: {workspace}
"#
    );
    let mut f = tempfile::NamedTempFile::new().expect("create temp config");
    std::io::Write::write_all(&mut f, config.as_bytes()).expect("write config");
    f
}

/// Config with bundles but no profiles — for testing empty tools/list.
fn write_empty_profiles_config() -> tempfile::NamedTempFile {
    let config = r#"bundles:
  baseline:
    binaries: [sh, bash, echo]
"#;
    let mut f = tempfile::NamedTempFile::new().expect("create temp config");
    std::io::Write::write_all(&mut f, config.as_bytes()).expect("write config");
    f
}
