//! Unit tests: Built-in credential presets (V2).
//!
//! Validates that built-in presets (gcloud, github, aws) resolve to the
//! correct provider config, and that unknown preset names produce errors.

/// V2 Slice 1: gcloud preset has correct command and inject mapping
/// When `builtin_presets()` is called,
/// Then the "gcloud" entry is a command provider with
/// `gcloud auth print-access-token` and injects CLOUDSDK_AUTH_ACCESS_TOKEN.
#[test]
fn gcloud_preset_has_correct_command_and_inject() {
    // Arrange + Act
    let presets = ostia_core::credentials::builtin_presets();

    // Assert
    let gcloud = presets.get("gcloud").expect("gcloud preset should exist");
    assert_eq!(gcloud.provider, "command");
    assert_eq!(
        gcloud.command.as_deref(),
        Some("gcloud auth print-access-token"),
        "gcloud preset should shell out to gcloud auth print-access-token"
    );
    assert_eq!(
        gcloud.inject.get("CLOUDSDK_AUTH_ACCESS_TOKEN").map(|s| s.as_str()),
        Some("value"),
        "gcloud preset should inject CLOUDSDK_AUTH_ACCESS_TOKEN from provider output 'value'"
    );
}

/// V2 Slice 1 (error case): Unknown preset name produces error
/// When a config references `fakename: preset`,
/// Then profile resolution fails with an error mentioning the unknown preset.
#[test]
fn unknown_preset_produces_error() {
    // Arrange
    let yaml = r#"
bundles:
  baseline:
    binaries: [echo]

profiles:
  test:
    bundles: [baseline]
    credentials:
      fakename: preset
"#;
    let mut f = tempfile::NamedTempFile::new().expect("create temp config");
    std::io::Write::write_all(&mut f, yaml.as_bytes()).expect("write config");

    // Act — load config and try to resolve profile
    let config = ostia_core::OstiaConfig::load(f.path()).expect("config should parse");
    let result = config.resolve_profile("test");

    // Assert
    assert!(
        result.is_err(),
        "unknown preset 'fakename' should produce an error"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("fakename") || err.contains("preset") || err.contains("unknown"),
        "error should mention the unknown preset, got: {:?}",
        err
    );
}
