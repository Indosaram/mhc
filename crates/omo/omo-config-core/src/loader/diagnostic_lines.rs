use crate::loader::types::{
    DIAGNOSTIC_DEPRECATED_KEYS, DIAGNOSTIC_INVALID_VALUE, DIAGNOSTIC_PARSE, DIAGNOSTIC_PROFILE,
    DIAGNOSTIC_READ, DIAGNOSTIC_UNKNOWN_KEYS, DIAGNOSTIC_VALIDATION, MERGED_OMO_CONFIG_PATH,
    OmoConfigDiagnostic,
};

fn slashed(value: &str) -> String {
    value.replace('\\', "/")
}

fn has_drive_prefix(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}

/// A config path as a doctor shows it: home-relative (`~/.omo/omo.jsonc`) when it lives under `homeDir`.
/// The part after `~` always uses forward slashes, so a warning reads the same on every platform.
/// A path outside the home directory keeps its native form.
pub fn display_omo_config_path(path: &str, home_dir: Option<&str>) -> String {
    if path == MERGED_OMO_CONFIG_PATH {
        return "merged config".to_string();
    }
    let Some(home_dir) = home_dir else {
        return path.to_string();
    };
    if home_dir.is_empty() {
        return path.to_string();
    }
    // Compare with one separator: a Windows home can arrive in POSIX form (C:/Users/me) while the
    // file path is native (C:\Users\me\...), and drive-letter paths compare case-insensitively.
    let home = slashed(home_dir);
    let home = home.trim_end_matches('/');
    let file = slashed(path);
    let fold = |value: &str| -> String {
        if has_drive_prefix(home) {
            value.to_lowercase()
        } else {
            value.to_string()
        }
    };
    let folded_home = fold(home);
    let folded_file = fold(&file);
    if folded_file == folded_home {
        return "~".to_string();
    }
    if folded_file.starts_with(&format!("{folded_home}/"))
        && let Some(tail) = file.get(home.len()..)
    {
        return format!("~{tail}");
    }
    path.to_string()
}

fn not_loaded_reason(diagnostic: &OmoConfigDiagnostic) -> String {
    match diagnostic.kind {
        DIAGNOSTIC_PARSE => "JSONC parse error".to_string(),
        DIAGNOSTIC_READ => "unreadable".to_string(),
        _ => {
            if diagnostic.issue_paths.is_empty() {
                "invalid config".to_string()
            } else {
                format!("invalid: {}", diagnostic.issue_paths.join(", "))
            }
        }
    }
}

/// Doctor lines, without a severity prefix, for every key the loader ignored and every file it did
/// not load: one line per dropped key, e.g. `config: ~/.omo/omo.jsonc: task.host_engine_policy ignored
/// (invalid value)`. Deprecated-key and profile notices are reported by their own surfaces.
pub fn omo_config_diagnostic_lines(
    diagnostics: &[OmoConfigDiagnostic],
    home_dir: Option<&str>,
) -> Vec<String> {
    let mut lines = Vec::new();
    for diagnostic in diagnostics {
        let file = display_omo_config_path(&diagnostic.path, home_dir);
        match diagnostic.kind {
            DIAGNOSTIC_INVALID_VALUE => {
                for key in &diagnostic.issue_paths {
                    lines.push(format!("config: {file}: {key} ignored (invalid value)"));
                }
            }
            DIAGNOSTIC_UNKNOWN_KEYS => {
                for key in &diagnostic.issue_paths {
                    lines.push(format!("config: {file}: {key} ignored (unknown key)"));
                }
            }
            DIAGNOSTIC_PARSE | DIAGNOSTIC_READ | DIAGNOSTIC_VALIDATION => {
                let reason = not_loaded_reason(diagnostic);
                if diagnostic.path == MERGED_OMO_CONFIG_PATH {
                    lines.push(format!("config: {file}: reset to defaults ({reason})"));
                } else {
                    lines.push(format!("config: {file}: not loaded ({reason})"));
                }
            }
            DIAGNOSTIC_DEPRECATED_KEYS | DIAGNOSTIC_PROFILE => {}
            _ => {}
        }
    }
    lines
}
