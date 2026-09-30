//! Port of senpi packages/ai/src/api/cursor-agent/pi-args.ts.
// ported by todo 12

use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PiReadArgs {
    pub path: String,
    pub offset: Option<i64>,
    pub limit: Option<i64>,
}

pub fn pi_read_args(path: &str, offset: Option<f64>, limit: Option<f64>) -> Option<PiReadArgs> {
    if let Some(limit) = limit
        && limit.floor() <= 0.0
    {
        return None;
    }
    Some(PiReadArgs {
        path: path.to_string(),
        offset: offset.map(|value| value.max(1.0).floor() as i64),
        limit: limit.map(|value| value.floor() as i64),
    })
}

pub fn pi_ls_path(base_path: Option<&str>) -> String {
    match base_path {
        Some(path) if !path.is_empty() => path.to_string(),
        _ => ".".to_string(),
    }
}

pub fn pi_limit(limit: Option<f64>) -> Option<i64> {
    limit.map(|value| value.max(1.0).floor() as i64)
}

pub fn pi_timeout(timeout: Option<f64>) -> Option<f64> {
    timeout.filter(|value| *value >= 0.0)
}

pub fn compose_shell_command(command: &str, working_directory: Option<&str>) -> String {
    let Some(working_directory) = working_directory.filter(|dir| !dir.is_empty()) else {
        return command.to_string();
    };
    let quoted = format!("'{}'", working_directory.replace('\'', "'\\''"));
    format!("cd {quoted} && {{ {command}\n}}")
}

pub fn omit_undefined_args(args: Map<String, Value>) -> Map<String, Value> {
    args.into_iter().filter(|(_, value)| !value.is_null()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn given_offset_and_limit_when_building_read_args_then_maps_directly() {
        assert_eq!(
            pi_read_args("a.ts", Some(3.0), Some(10.0)),
            Some(PiReadArgs { path: "a.ts".to_string(), offset: Some(3), limit: Some(10) })
        );
    }

    #[test]
    fn given_zero_limit_when_building_read_args_then_returns_none() {
        assert_eq!(pi_read_args("a.ts", None, Some(0.0)), None);
    }

    #[test]
    fn given_zero_offset_when_building_read_args_then_clamps_to_one() {
        assert_eq!(
            pi_read_args("a.ts", Some(0.0), None),
            Some(PiReadArgs { path: "a.ts".to_string(), offset: Some(1), limit: None })
        );
    }

    #[test]
    fn given_zero_timeout_when_resolved_then_preserved() {
        assert_eq!(pi_timeout(Some(0.0)), Some(0.0));
    }

    #[test]
    fn given_negative_timeout_when_resolved_then_falls_back_to_default() {
        assert_eq!(pi_timeout(Some(-5.0)), None);
    }

    #[test]
    fn given_absent_timeout_when_resolved_then_stays_absent() {
        assert_eq!(pi_timeout(None), None);
    }

    #[test]
    fn given_no_working_directory_when_composing_command_then_returns_bare_command() {
        assert_eq!(compose_shell_command("ls", None), "ls");
    }

    #[test]
    fn given_working_directory_with_quote_when_composing_command_then_escapes_it() {
        assert_eq!(
            compose_shell_command("ls", Some("/tmp/it's here")),
            "cd '/tmp/it'\\''s here' && { ls\n}"
        );
    }

    #[test]
    fn given_mixed_undefined_and_present_values_when_omitted_then_drops_nulls_only() {
        let mut args = Map::new();
        args.insert("a".to_string(), json!(1));
        args.insert("b".to_string(), Value::Null);
        args.insert("c".to_string(), json!("x"));
        let mut expected = Map::new();
        expected.insert("a".to_string(), json!(1));
        expected.insert("c".to_string(), json!("x"));
        assert_eq!(omit_undefined_args(args), expected);
    }

    #[test]
    fn given_absent_base_path_when_resolving_ls_path_then_defaults_to_dot() {
        assert_eq!(pi_ls_path(None), ".");
        assert_eq!(pi_ls_path(Some("")), ".");
    }

    #[test]
    fn given_present_base_path_when_resolving_ls_path_then_returns_it() {
        assert_eq!(pi_ls_path(Some("/tmp")), "/tmp");
    }

    #[test]
    fn given_fractional_limit_when_clamped_then_floors_and_enforces_minimum() {
        assert_eq!(pi_limit(Some(3.7)), Some(3));
        assert_eq!(pi_limit(Some(0.2)), Some(1));
        assert_eq!(pi_limit(None), None);
    }
}
