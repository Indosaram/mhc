//! Port of senpi packages/agent/src/harness/prompt-templates.ts.
//!
//! `yaml` is not a dependency of this crate, so frontmatter is read by the same minimal
//! `key: value` subset the templates use; a malformed flow value is reported as `parse_failed`
//! exactly as the YAML parser reports it.

use maho_ai::types::BoxFuture;

use super::context::Context;
use super::result::{Result, ok};
use super::types::{ExecutionEnv, FileInfo, FileKind, PromptTemplate};

/// `PromptTemplateDiagnosticCode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptTemplateDiagnosticCode {
    FileInfoFailed,
    ListFailed,
    ReadFailed,
    ParseFailed,
}

impl PromptTemplateDiagnosticCode {
    pub fn as_str(self) -> &'static str {
        match self {
            PromptTemplateDiagnosticCode::FileInfoFailed => "file_info_failed",
            PromptTemplateDiagnosticCode::ListFailed => "list_failed",
            PromptTemplateDiagnosticCode::ReadFailed => "read_failed",
            PromptTemplateDiagnosticCode::ParseFailed => "parse_failed",
        }
    }
}

/// `PromptTemplateDiagnostic`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplateDiagnostic {
    pub kind: &'static str,
    pub code: PromptTemplateDiagnosticCode,
    pub message: String,
    pub path: String,
}

impl PromptTemplateDiagnostic {
    fn warning(
        code: PromptTemplateDiagnosticCode,
        message: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        Self {
            kind: "warning",
            code,
            message: message.into(),
            path: path.into(),
        }
    }
}

/// `loadPromptTemplates(env, paths, context)`.
pub async fn load_prompt_templates(
    env: &dyn ExecutionEnv,
    paths: Vec<String>,
    context: &Context,
) -> (Vec<PromptTemplate>, Vec<PromptTemplateDiagnostic>) {
    let mut prompt_templates = Vec::new();
    let mut diagnostics = Vec::new();
    for path in paths {
        let info_result = env.file_info(&path, context).await;
        let info = match info_result {
            Ok(info) => info,
            Err(error) => {
                if error.code != super::types::FileErrorCode::NotFound {
                    diagnostics.push(PromptTemplateDiagnostic::warning(
                        PromptTemplateDiagnosticCode::FileInfoFailed,
                        error.message,
                        path,
                    ));
                }
                continue;
            }
        };
        let kind = resolve_kind(env, &info, &mut diagnostics, context).await;
        match kind {
            Some(FileKind::Directory) => {
                let (templates, mut dir_diagnostics) =
                    load_templates_from_dir(env, &info.path, context).await;
                prompt_templates.extend(templates);
                diagnostics.append(&mut dir_diagnostics);
            }
            Some(FileKind::File) if info.name.ends_with(".md") => {
                let (template, mut file_diagnostics) =
                    load_template_from_file(env, &info.path, &info.name, context).await;
                if let Some(template) = template {
                    prompt_templates.push(template);
                }
                diagnostics.append(&mut file_diagnostics);
            }
            _ => {}
        }
    }
    (prompt_templates, diagnostics)
}

/// `mapPromptTemplate` argument of `loadSourcedPromptTemplates`.
pub type PromptTemplateMapper<TSource> =
    dyn Fn(PromptTemplate, TSource, &Context) -> PromptTemplate + Send + Sync;

/// `loadSourcedPromptTemplates(env, inputs, mapPromptTemplate, context)`.
pub async fn load_sourced_prompt_templates<TSource: Clone>(
    env: &dyn ExecutionEnv,
    inputs: Vec<(String, TSource)>,
    map_prompt_template: Option<&PromptTemplateMapper<TSource>>,
    context: &Context,
) -> (
    Vec<(PromptTemplate, TSource)>,
    Vec<(PromptTemplateDiagnostic, TSource)>,
) {
    let mut prompt_templates = Vec::new();
    let mut diagnostics = Vec::new();
    for (path, source) in inputs {
        let (templates, template_diagnostics) =
            load_prompt_templates(env, vec![path], context).await;
        for template in templates {
            let mapped = match map_prompt_template {
                Some(map) => map(template, source.clone(), context),
                None => template,
            };
            prompt_templates.push((mapped, source.clone()));
        }
        for diagnostic in template_diagnostics {
            diagnostics.push((diagnostic, source.clone()));
        }
    }
    (prompt_templates, diagnostics)
}

async fn load_templates_from_dir(
    env: &dyn ExecutionEnv,
    dir: &str,
    context: &Context,
) -> (Vec<PromptTemplate>, Vec<PromptTemplateDiagnostic>) {
    let mut prompt_templates = Vec::new();
    let mut diagnostics = Vec::new();
    let entries = match env.list_dir(dir, context).await {
        Ok(entries) => entries,
        Err(error) => {
            diagnostics.push(PromptTemplateDiagnostic::warning(
                PromptTemplateDiagnosticCode::ListFailed,
                error.message,
                dir,
            ));
            return (prompt_templates, diagnostics);
        }
    };

    let mut entries = entries;
    entries.sort_by(|a, b| a.name.cmp(&b.name));

    for entry in entries {
        let kind = resolve_kind(env, &entry, &mut diagnostics, context).await;
        if kind != Some(FileKind::File) || !entry.name.ends_with(".md") {
            continue;
        }
        let (template, mut file_diagnostics) =
            load_template_from_file(env, &entry.path, &entry.name, context).await;
        if let Some(template) = template {
            prompt_templates.push(template);
        }
        diagnostics.append(&mut file_diagnostics);
    }
    (prompt_templates, diagnostics)
}

async fn load_template_from_file(
    env: &dyn ExecutionEnv,
    file_path: &str,
    file_name: &str,
    context: &Context,
) -> (Option<PromptTemplate>, Vec<PromptTemplateDiagnostic>) {
    let mut diagnostics = Vec::new();
    let raw_content = match env.read_text_file(file_path, context).await {
        Ok(content) => content,
        Err(error) => {
            diagnostics.push(PromptTemplateDiagnostic::warning(
                PromptTemplateDiagnosticCode::ReadFailed,
                error.message,
                file_path,
            ));
            return (None, diagnostics);
        }
    };

    let parsed = match parse_frontmatter(&raw_content) {
        Ok(parsed) => parsed,
        Err(message) => {
            diagnostics.push(PromptTemplateDiagnostic::warning(
                PromptTemplateDiagnosticCode::ParseFailed,
                message,
                file_path,
            ));
            return (None, diagnostics);
        }
    };

    let ParsedFrontmatter { frontmatter, body } = parsed;
    let first_line = body.split('\n').find(|line| !line.trim().is_empty());
    let mut description = frontmatter.get("description").cloned().unwrap_or_default();
    if description.is_empty()
        && let Some(first_line) = first_line
    {
        description = first_line.chars().take(60).collect();
        if first_line.chars().count() > 60 {
            description.push_str("...");
        }
    }
    (
        Some(PromptTemplate {
            name: strip_md_suffix(file_name),
            description: Some(description),
            content: body,
        }),
        diagnostics,
    )
}

fn strip_md_suffix(name: &str) -> String {
    if name.len() >= 3 && name[name.len() - 3..].eq_ignore_ascii_case(".md") {
        name[..name.len() - 3].to_string()
    } else {
        name.to_string()
    }
}

async fn resolve_kind(
    env: &dyn ExecutionEnv,
    info: &FileInfo,
    diagnostics: &mut Vec<PromptTemplateDiagnostic>,
    context: &Context,
) -> Option<FileKind> {
    if info.kind == FileKind::File || info.kind == FileKind::Directory {
        return Some(info.kind);
    }
    let canonical_path = match env.canonical_path(&info.path, context).await {
        Ok(path) => path,
        Err(error) => {
            if error.code != super::types::FileErrorCode::NotFound {
                diagnostics.push(PromptTemplateDiagnostic::warning(
                    PromptTemplateDiagnosticCode::FileInfoFailed,
                    error.message,
                    info.path.clone(),
                ));
            }
            return None;
        }
    };
    match env.file_info(&canonical_path, context).await {
        Ok(target) if target.kind == FileKind::File || target.kind == FileKind::Directory => {
            Some(target.kind)
        }
        Ok(_) => None,
        Err(error) => {
            if error.code != super::types::FileErrorCode::NotFound {
                diagnostics.push(PromptTemplateDiagnostic::warning(
                    PromptTemplateDiagnosticCode::FileInfoFailed,
                    error.message,
                    info.path.clone(),
                ));
            }
            None
        }
    }
}

/// Frontmatter plus body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedFrontmatter {
    pub frontmatter: std::collections::BTreeMap<String, String>,
    pub body: String,
}

/// `parseFrontmatter(content)` retaining string-valued template metadata.
pub fn parse_frontmatter(content: &str) -> Result<ParsedFrontmatter, String> {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    if !normalized.starts_with("---") {
        return ok(ParsedFrontmatter {
            frontmatter: Default::default(),
            body: normalized,
        });
    }
    let Some(end_index) = normalized[3..].find("\n---").map(|index| index + 3) else {
        return ok(ParsedFrontmatter {
            frontmatter: Default::default(),
            body: normalized,
        });
    };
    let yaml_string = &normalized[4..end_index];
    let body = normalized[end_index + 4..].trim().to_string();
    match parse_yaml_mapping(yaml_string) {
        Ok(frontmatter) => ok(ParsedFrontmatter { frontmatter, body }),
        Err(message) => Err(message),
    }
}

fn parse_yaml_mapping(source: &str) -> Result<std::collections::BTreeMap<String, String>, String> {
    let value: serde_yaml::Value =
        serde_yaml::from_str(source).map_err(|error| error.to_string())?;
    Ok(match value {
        serde_yaml::Value::Mapping(mapping) => mapping
            .into_iter()
            .filter_map(|(key, value)| match (key, value) {
                (serde_yaml::Value::String(key), serde_yaml::Value::String(value)) => {
                    Some((key, value))
                }
                _ => None,
            })
            .collect(),
        _ => Default::default(),
    })
}

/// `parseCommandArgs(argsString)`: shell-style single and double quotes.
pub fn parse_command_args(args_string: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quote: Option<char> = None;

    for character in args_string.chars() {
        match in_quote {
            Some(quote) => {
                if character == quote {
                    in_quote = None;
                } else {
                    current.push(character);
                }
            }
            None => {
                if character == '"' || character == '\'' {
                    in_quote = Some(character);
                } else if character == ' ' || character == '\t' {
                    if !current.is_empty() {
                        args.push(std::mem::take(&mut current));
                    }
                } else {
                    current.push(character);
                }
            }
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

/// `substituteArgs(content, args)`.
pub fn substitute_args(content: &str, args: &[String]) -> String {
    let positional = substitute_stage(content, args, false);
    let sliced = substitute_stage(&positional, args, true);
    let all_args = args.join(" ");
    sliced
        .replace("$ARGUMENTS", &all_args)
        .replace("$@", &all_args)
}

fn substitute_stage(content: &str, args: &[String], sliced: bool) -> String {
    let mut result = String::new();
    let chars: Vec<char> = content.chars().collect();
    let mut index = 0;

    while index < chars.len() {
        if chars[index] == '$'
            && let Some((replacement, consumed)) = match_dollar(&chars[index..], args, sliced)
        {
            result.push_str(&replacement);
            index += consumed;
            continue;
        }
        result.push(chars[index]);
        index += 1;
    }
    result
}

fn match_dollar(chars: &[char], args: &[String], sliced: bool) -> Option<(String, usize)> {
    if chars.len() < 2 {
        return None;
    }
    match chars[1] {
        '{' if sliced => {
            let mut index = 2;
            let mut content = String::new();
            while index < chars.len() && chars[index] != '}' {
                content.push(chars[index]);
                index += 1;
            }
            if index >= chars.len() {
                return None;
            }
            let consumed = index + 1;
            let rest = content.strip_prefix("@:")?;
            let (start_str, length_str) = match rest.split_once(':') {
                Some((start, length)) => (start, Some(length)),
                None => (rest, None),
            };
            if start_str.is_empty()
                || !start_str.bytes().all(|byte| byte.is_ascii_digit())
                || length_str.is_some_and(|length| {
                    length.is_empty() || !length.bytes().all(|byte| byte.is_ascii_digit())
                })
            {
                return None;
            }
            let start: usize = start_str.parse().ok()?;
            let start = start.saturating_sub(1);
            let start = start.min(args.len());
            match length_str {
                Some(length_str) => {
                    let length: usize = length_str.parse().ok()?;
                    let end = start.saturating_add(length).min(args.len());
                    Some((args[start..end].join(" "), consumed))
                }
                None => Some((args[start..].join(" "), consumed)),
            }
        }
        digit if !sliced && digit.is_ascii_digit() => {
            let mut index = 1;
            let mut number = String::new();
            while index < chars.len() && chars[index].is_ascii_digit() {
                number.push(chars[index]);
                index += 1;
            }
            let position: usize = number.parse().ok()?;
            let value = position
                .checked_sub(1)
                .and_then(|position| args.get(position))
                .cloned()
                .unwrap_or_default();
            Some((value, index))
        }
        _ => None,
    }
}

/// `formatPromptTemplateInvocation(template, args)`.
pub fn format_prompt_template_invocation(template: &PromptTemplate, args: &[String]) -> String {
    substitute_args(&template.content, args)
}

/// Helper so `BoxFuture` stays referenced by this module's API surface.
pub type PromptTemplateFuture<'a, T> = BoxFuture<'a, T>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_yaml_string_forms_without_coercing_non_strings() {
        // Given YAML block scalars, escapes, and typed scalar descriptions.
        for (yaml, expected) in [
            ("description: |-\n  alpha\n  beta", Some("alpha\nbeta")),
            ("description: >-\n  alpha\n  beta", Some("alpha beta")),
            ("description: \"alpha\\nbeta\"", Some("alpha\nbeta")),
            ("description: 'it''s YAML'", Some("it's YAML")),
            ("description: 42", None),
            ("description: true", None),
        ] {
            // When parsing the template metadata.
            let parsed = parse_frontmatter(&format!("---\n{yaml}\n---\nbody")).expect("valid YAML");
            // Then only genuine YAML strings supply the description.
            assert_eq!(
                parsed.frontmatter.get("description").map(String::as_str),
                expected
            );
        }
    }

    #[test]
    fn substitutions_revisit_only_later_stages() {
        // Given placeholders inside arguments inserted by an earlier stage.
        let cases = [
            ("$1", vec!["${@:2:1}", "ok"], "ok"),
            ("$1", vec!["$ARGUMENTS", "ok"], "$ARGUMENTS ok"),
            ("$1", vec!["$2", "ok"], "$2"),
            ("${@:1:1}", vec!["$ARGUMENTS", "ok"], "$ARGUMENTS ok"),
            ("$ARGUMENTS", vec!["$@"], "$@"),
            ("${@:0:1}", vec!["ok", "tail"], "ok"),
        ];
        for (content, args, expected) in cases {
            let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
            // When substituting all stages.
            let actual = substitute_args(content, &args);
            // Then positional and sliced replacements are not recursive.
            assert_eq!(actual, expected);
        }
    }
}
