use std::collections::HashSet;
use maho_tools::definition::KernelPrelude;
use crate::bridges::schema_bridge::EvalSchemaToolInfo;

pub type ActiveToolsSource = std::sync::Arc<dyn Fn() -> Result<Vec<String>, String> + Send + Sync>;
pub type AllToolsSource = std::sync::Arc<dyn Fn() -> Result<Vec<EvalSchemaToolInfo>, String> + Send + Sync>;

pub const KERNEL_BUILTIN_GLOBALS: &[&str] = &[
    "agent",
    "completion",
    "display",
    "env",
    "log",
    "output",
    "parallel",
    "phase",
    "pipeline",
    "print",
    "read",
    "tool",
    "tool_schema",
    "tools",
    "workpool",
    "write",
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Tool \"{tool_name}\" kernelPrelude export \"{colliding_export}\" collides with a built-in eval kernel global.")]
pub struct KernelPreludeCollisionError {
    pub tool_name: String,
    pub colliding_export: String,
}

pub fn project_kernel_prelude(tool_name: &str, prelude: Option<KernelPrelude>) -> Result<Option<KernelPrelude>, KernelPreludeCollisionError> {
    if let Some(prelude) = prelude {
        let builtins: HashSet<&str> = KERNEL_BUILTIN_GLOBALS.iter().copied().collect();
        for export in &prelude.exports {
            if builtins.contains(export.as_str()) || export.starts_with("__") {
                return Err(KernelPreludeCollisionError {
                    tool_name: tool_name.to_owned(),
                    colliding_export: export.clone(),
                });
            }
        }
        Ok(Some(prelude))
    } else {
        Ok(None)
    }
}

pub fn active_kernel_preludes(
    active_tools: &[String],
    all_tools: &[EvalSchemaToolInfo],
) -> Vec<KernelPrelude> {
    let active_set: HashSet<&str> = active_tools.iter().map(|s| s.as_str()).collect();
    all_tools
        .iter()
        .filter_map(|tool| {
            if active_set.contains(tool.name.as_str()) {
                tool.kernel_prelude.clone()
            } else {
                None
            }
        })
        .collect()
}

pub fn kernel_prelude_docs_key(preludes: &[KernelPrelude]) -> String {
    preludes
        .iter()
        .map(|p| p.documentation.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn prompt_kernel_preludes(
    active_tools: Option<&[String]>,
    all_tools: Option<&[EvalSchemaToolInfo]>,
) -> Vec<KernelPrelude> {
    match (active_tools, all_tools) {
        (Some(active), Some(all)) => active_kernel_preludes(active, all),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_kernel_prelude_validates_collisions() {
        let valid = KernelPrelude {
            javascript: "globalThis.calc = {};".into(),
            python: "calc = {}".into(),
            documentation: "calc: calculator facade".into(),
            exports: vec!["calc".into()],
            ..Default::default()
        };
        assert!(project_kernel_prelude("calculator", Some(valid.clone())).is_ok());

        let builtin_collision = KernelPrelude {
            javascript: String::new(),
            python: String::new(),
            documentation: String::new(),
            exports: vec!["display".into()],
            ..Default::default()
        };
        let err = project_kernel_prelude("bad_tool", Some(builtin_collision)).unwrap_err();
        assert_eq!(err.tool_name, "bad_tool");
        assert_eq!(err.colliding_export, "display");

        let internal_collision = KernelPrelude {
            javascript: String::new(),
            python: String::new(),
            documentation: String::new(),
            exports: vec!["__private".into()],
            ..Default::default()
        };
        let err2 = project_kernel_prelude("secret_tool", Some(internal_collision)).unwrap_err();
        assert_eq!(err2.colliding_export, "__private");
    }

    #[test]
    fn active_preludes_filters_inactive_tools() {
        let p1 = KernelPrelude {
            javascript: "globalThis.tool1 = true;".into(),
            python: "tool1 = True".into(),
            documentation: "tool1 facade".into(),
            exports: vec!["tool1".into()],
            ..Default::default()
        };
        let p2 = KernelPrelude {
            javascript: "globalThis.tool2 = true;".into(),
            python: "tool2 = True".into(),
            documentation: "tool2 facade".into(),
            exports: vec!["tool2".into()],
            ..Default::default()
        };
        let all = vec![
            EvalSchemaToolInfo {
                name: "t1".into(),
                description: None,
                parameters: None,
                kernel_prelude: Some(p1.clone()),
            },
            EvalSchemaToolInfo {
                name: "t2".into(),
                description: None,
                parameters: None,
                kernel_prelude: Some(p2.clone()),
            },
            EvalSchemaToolInfo {
                name: "t3".into(),
                description: None,
                parameters: None,
                kernel_prelude: None,
            },
        ];
        let active = vec!["t1".into()];
        let preludes = active_kernel_preludes(&active, &all);
        assert_eq!(preludes.len(), 1);
        assert_eq!(preludes[0].exports, vec!["tool1".to_string()]);
    }
}
