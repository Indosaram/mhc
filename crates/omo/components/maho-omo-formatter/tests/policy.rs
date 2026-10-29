use maho_omo_formatter::*;
use std::collections::HashMap;
use std::path::Path;

#[test]
fn test_marker_precedence_and_never_enables_prettier_beside_biome() {
    let markers_biome = vec!["biome.json".to_string(), ".prettierrc".to_string()];
    let res = detect_formatter(&markers_biome, Path::new("src/a.ts"), "");
    assert_eq!(res.unwrap().tool, FormatterTool::Biome);

    let markers_prettier = vec![".prettierrc".to_string()];
    let res = detect_formatter(&markers_prettier, Path::new("src/a.ts"), "");
    assert_eq!(res.unwrap().tool, FormatterTool::Prettier);

    let markers_cargo = vec!["Cargo.toml".to_string()];
    let res = detect_formatter(&markers_cargo, Path::new("src/a.rs"), "");
    assert_eq!(res.unwrap().tool, FormatterTool::Rustfmt);

    let markers_go = vec!["go.mod".to_string()];
    let res = detect_formatter(&markers_go, Path::new("src/a.go"), "");
    assert_eq!(res.unwrap().tool, FormatterTool::Gofmt);

    let markers_py = vec!["pyproject.toml".to_string()];
    let res = detect_formatter(&markers_py, Path::new("src/a.py"), "[tool.ruff]\nline-length=88");
    assert_eq!(res.unwrap().tool, FormatterTool::Ruff);

    let res_none = detect_formatter(&markers_py, Path::new("src/a.py"), "[tool.black]\nline-length=88");
    assert!(res_none.is_none());
}

#[test]
fn test_format_on_mutation_defaults_and_language_overrides() {
    let defaults = FormatOnMutationConfig::default();
    assert_eq!(defaults.mode, Some(FormatMode::BestEffort));
    assert_eq!(defaults.max_file_bytes, Some(1_048_576));
    assert_eq!(defaults.timeout_ms, Some(3_000));

    let mut langs = HashMap::new();
    langs.insert("typescript".to_string(), false);
    let config = FormatOnMutationConfig {
        mode: Some(FormatMode::Required),
        languages: langs,
        max_file_bytes: None,
        timeout_ms: None,
    };
    assert_eq!(resolve_format_mode(&config, "typescript"), FormatMode::Off);
    assert_eq!(resolve_format_mode(&config, "python"), FormatMode::Required);

    let empty = FormatOnMutationConfig::default();
    assert_eq!(resolve_format_mode(&empty, "python"), FormatMode::BestEffort);
}

#[test]
fn test_change_notice_tokens_in_message() {
    let notice = "(OmO) auto-formatted src/a.ts with biome (+1/-2 lines). File content changed; re-read before exact-text edits.";
    assert!(notice.contains("auto-formatted"));
    assert!(notice.contains("re-read before exact-text edits"));
}

#[test]
fn test_format_on_mutation_config_deserialization_camel_case() {
    let raw = serde_json::json!({
        "mode": "required",
        "languages": { "typescript": false, "python": true },
        "maxFileBytes": 524288,
        "timeoutMs": 2500
    });
    let parsed: FormatOnMutationConfig = serde_json::from_value(raw).unwrap();
    assert_eq!(parsed.mode, Some(FormatMode::Required));
    assert_eq!(parsed.languages.get("typescript"), Some(&false));
    assert_eq!(parsed.languages.get("python"), Some(&true));
    assert_eq!(parsed.max_file_bytes, Some(524288));
    assert_eq!(parsed.timeout_ms, Some(2500));
}
