//! Port of senpi `packages/coding-agent/src/core/prompt-templates.ts`.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use crate::config::config_dir_name;
use crate::diagnostics::{ResourceDiagnostic, ResourceDiagnosticType};
use crate::frontmatter::parse_frontmatter;
use crate::paths::{PathInputOptions, lexical_resolve, resolve_path};
use crate::source_info::{SourceInfo, SyntheticSourceInfoOptions, create_synthetic_source_info};

/// `PromptTemplate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplate {
    pub name: String,
    pub description: String,
    pub argument_hint: Option<String>,
    pub requires_arguments: bool,
    pub content: String,
    pub source_info: SourceInfo,
    pub file_path: String,
}

/// `parseCommandArgs`: splits on whitespace, honouring single and double quotes.
pub fn parse_command_args(args_string: &str) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_quote: Option<char> = None;

    for character in args_string.chars() {
        if let Some(quote) = in_quote {
            if character == quote {
                in_quote = None;
            } else {
                current.push(character);
            }
        } else if character == '"' || character == '\'' {
            in_quote = Some(character);
        } else if character.is_whitespace() {
            if !current.is_empty() {
                args.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
    }

    if !current.is_empty() {
        args.push(current);
    }

    args
}

static SUBSTITUTE_ARGS_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\$\{(\d+|ARGUMENTS|@):-([^}]*)\}|\$\{@:(\d+)(?::(\d+))?\}|\$(ARGUMENTS|@|\d+)")
        .expect("substitute-args pattern")
});

/// `substituteArgs`: positional, `$@`/`$ARGUMENTS`, defaulted and sliced placeholders.
pub fn substitute_args(content: &str, args: &[String]) -> String {
    let all_args = args.join(" ");

    SUBSTITUTE_ARGS_PATTERN
        .replace_all(content, |captures: &regex::Captures<'_>| {
            if let Some(target) = captures.get(1) {
                let target = target.as_str();
                let value = if target == "@" || target == "ARGUMENTS" {
                    Some(all_args.clone())
                } else {
                    target
                        .parse::<usize>()
                        .ok()
                        .and_then(|index| index.checked_sub(1))
                        .and_then(|index| args.get(index).cloned())
                };
                return match value {
                    Some(value) if !value.is_empty() => value,
                    _ => captures.get(2).map_or(String::new(), |group| group.as_str().to_string()),
                };
            }

            if let Some(slice_start) = captures.get(3) {
                let start = slice_start.as_str().parse::<i64>().unwrap_or(0) - 1;
                let start = if start < 0 { 0usize } else { start as usize };
                if let Some(slice_length) = captures.get(4) {
                    let length = slice_length.as_str().parse::<usize>().unwrap_or(0);
                    return args.iter().skip(start).take(length).cloned().collect::<Vec<_>>().join(" ");
                }
                return args.iter().skip(start).cloned().collect::<Vec<_>>().join(" ");
            }

            let Some(simple) = captures.get(5) else {
                return String::new();
            };
            let simple = simple.as_str();
            if simple == "ARGUMENTS" || simple == "@" {
                return all_args.clone();
            }
            match simple.parse::<usize>().unwrap_or(0) {
                0 => String::new(),
                index => args.get(index - 1).cloned().unwrap_or_default(),
            }
        })
        .into_owned()
}

fn first_non_empty_line(body: &str) -> Option<&str> {
    body.split('\n').find(|line| !line.trim().is_empty())
}

fn warning(message: String, path: &str) -> ResourceDiagnostic {
    ResourceDiagnostic { category: None, diagnostic_type: ResourceDiagnosticType::Warning, message, path: Some(path.into()), collision: None }
}
/// `loadTemplateFromFile`: returns diagnostics when a file cannot be read or parsed.
pub fn load_template_from_file(file_path: &str, source_info: SourceInfo) -> (Option<PromptTemplate>, Vec<ResourceDiagnostic>) {
    let raw_content = match std::fs::read_to_string(file_path) {
        Ok(content) => content,
        Err(error) => return (None, vec![warning(error.to_string(), file_path)]),
    };
    let parsed = match parse_frontmatter(&raw_content) {
        Ok(parsed) => parsed,
        Err(error) => return (None, vec![warning(error.to_string(), file_path)]),
    };

    let file_name = Path::new(file_path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = file_name.strip_suffix(".md").map(str::to_string).unwrap_or(file_name);

    let mut description = parsed
        .frontmatter
        .get("description")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    if description.is_empty()
        && let Some(first_line) = first_non_empty_line(&parsed.body)
    {
        let truncated: String = first_line.chars().take(60).collect();
        let was_truncated = first_line.chars().count() > 60;
        description = if was_truncated { format!("{truncated}...") } else { truncated };
    }

    let argument_hint = parsed
        .frontmatter
        .get("argument-hint")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let requires_arguments = parsed.frontmatter.get("requires-arguments")
        .and_then(|value| value.as_bool())
        .unwrap_or(argument_hint.is_some());
    (Some(PromptTemplate { name, description, argument_hint, requires_arguments, content: parsed.body, source_info, file_path: file_path.to_string() }), Vec::new())
}

/// `loadTemplatesFromDir`: non-recursive `.md` scan; broken symlinks are skipped.
pub fn load_templates_from_dir(dir: &str, get_source_info: &dyn Fn(&str) -> SourceInfo) -> (Vec<PromptTemplate>, Vec<ResourceDiagnostic>) {
    let mut templates: Vec<PromptTemplate> = Vec::new();
    let mut diagnostics = Vec::new();
    if !Path::new(dir).exists() {
        return (templates, diagnostics);
    }

    let Ok(entries) = crate::utils::read_dir_sorted(dir) else {
        return (templates, diagnostics);
    };

    for entry in entries {
        let full_path = entry.path().to_string_lossy().into_owned();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let is_file = if file_type.is_symlink() {
            match std::fs::metadata(&full_path) {
                Ok(metadata) => metadata.is_file(),
                Err(_) => continue,
            }
        } else {
            file_type.is_file()
        };

        if is_file && entry.file_name().to_string_lossy().ends_with(".md") {
            let (template, mut found) = load_template_from_file(&full_path, get_source_info(&full_path));
            if let Some(template) = template { templates.push(template); }
            diagnostics.append(&mut found);
        }
    }

    (templates, diagnostics)
}

/// `LoadPromptTemplatesOptions`.
#[derive(Debug, Clone, Default)]
pub struct LoadPromptTemplatesOptions {
    pub cwd: String,
    pub agent_dir: String,
    pub prompt_paths: Vec<String>,
    pub include_defaults: bool,
}

fn is_under_path(target: &str, root: &str) -> bool {
    let normalized_root = lexical_resolve(root);
    if target == normalized_root {
        return true;
    }
    let prefix = if normalized_root.ends_with(std::path::MAIN_SEPARATOR) {
        normalized_root
    } else {
        format!("{normalized_root}{}", std::path::MAIN_SEPARATOR)
    };
    target.starts_with(&prefix)
}

/// `loadPromptTemplates`: global, then project, then the explicit paths.
pub fn load_prompt_templates(options: &LoadPromptTemplatesOptions) -> (Vec<PromptTemplate>, Vec<ResourceDiagnostic>) {
    let process_cwd = std::env::current_dir().map(|path| path.to_string_lossy().into_owned()).unwrap_or_default();
    let resolved_cwd = resolve_path(&options.cwd, &process_cwd, &PathInputOptions::default());
    let resolved_agent_dir = resolve_path(&options.agent_dir, &process_cwd, &PathInputOptions::default());

    let mut templates: Vec<PromptTemplate> = Vec::new();
    let mut diagnostics = Vec::new();

    let global_prompts_dir = Path::new(&resolved_agent_dir).join("prompts").to_string_lossy().into_owned();
    let project_prompts_dir = lexical_resolve(
        &Path::new(&resolved_cwd).join(config_dir_name()).join("prompts").to_string_lossy(),
    );

    let get_source_info = |resolved_path: &str| -> SourceInfo {
        if is_under_path(resolved_path, &global_prompts_dir) {
            return create_synthetic_source_info(
                resolved_path,
                SyntheticSourceInfoOptions {
                    source: "local".to_string(),
                    scope: Some(crate::source_info::SourceScope::User),
                    origin: None,
                    base_dir: Some(global_prompts_dir.clone()),
                },
            );
        }
        if is_under_path(resolved_path, &project_prompts_dir) {
            return create_synthetic_source_info(
                resolved_path,
                SyntheticSourceInfoOptions {
                    source: "local".to_string(),
                    scope: Some(crate::source_info::SourceScope::Project),
                    origin: None,
                    base_dir: Some(project_prompts_dir.clone()),
                },
            );
        }
        let is_directory = std::fs::metadata(resolved_path).map(|metadata| metadata.is_dir()).unwrap_or(false);
        let base_dir = if is_directory {
            resolved_path.to_string()
        } else {
            Path::new(resolved_path)
                .parent()
                .map(|parent| parent.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        create_synthetic_source_info(
            resolved_path,
            SyntheticSourceInfoOptions {
                source: "local".to_string(),
                base_dir: Some(base_dir),
                ..Default::default()
            },
        )
    };

    if options.include_defaults {
        let (loaded, mut found) = load_templates_from_dir(&global_prompts_dir, &get_source_info);
        templates.extend(loaded);
        diagnostics.append(&mut found);
        let (loaded, mut found) = load_templates_from_dir(&project_prompts_dir, &get_source_info);
        templates.extend(loaded);
        diagnostics.append(&mut found);
    }

    for raw_path in &options.prompt_paths {
        let resolved_path = resolve_path(raw_path, &resolved_cwd, &PathInputOptions { trim: true, ..Default::default() });
        if !Path::new(&resolved_path).exists() {
            continue;
        }

        let metadata = match std::fs::metadata(&resolved_path) {
            Ok(metadata) => metadata,
            Err(error) => { diagnostics.push(warning(error.to_string(), &resolved_path)); continue; }
        };
        if metadata.is_dir() {
            let (loaded, mut found) = load_templates_from_dir(&resolved_path, &get_source_info);
            templates.extend(loaded);
            diagnostics.append(&mut found);
        } else if metadata.is_file()
            && resolved_path.ends_with(".md")
        {
            let (template, mut found) = load_template_from_file(&resolved_path, get_source_info(&resolved_path));
            if let Some(template) = template { templates.push(template); }
            diagnostics.append(&mut found);
        }
    }

    (templates, diagnostics)
}

/// `PromptTemplateExpansion`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplateExpansion {
    pub text: String,
    pub template: Option<PromptTemplate>,
}

static EXPAND_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^/(\S+)(?:\s+([\s\S]*))?$").expect("expand pattern"));

/// `expandPromptTemplateWithMetadata`.
pub fn expand_prompt_template_with_metadata(text: &str, templates: &[PromptTemplate]) -> PromptTemplateExpansion {
    if !text.starts_with('/') {
        return PromptTemplateExpansion { text: text.to_string(), template: None };
    }

    let Some(captures) = EXPAND_PATTERN.captures(text) else {
        return PromptTemplateExpansion { text: text.to_string(), template: None };
    };

    let template_name = captures.get(1).map_or("", |group| group.as_str());
    let args_string = captures.get(2).map_or("", |group| group.as_str());

    let Some(template) = templates.iter().find(|template| template.name == template_name) else {
        return PromptTemplateExpansion { text: text.to_string(), template: None };
    };

    let args = parse_command_args(args_string);
    PromptTemplateExpansion { text: substitute_args(&template.content, &args), template: Some(template.clone()) }
}

/// `expandPromptTemplate`.
pub fn expand_prompt_template(text: &str, templates: &[PromptTemplate]) -> String {
    expand_prompt_template_with_metadata(text, templates).text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_info::SourceScope;

    fn synthetic(path: &str) -> SourceInfo {
        create_synthetic_source_info(path, SyntheticSourceInfoOptions { source: "local".to_string(), ..Default::default() })
    }

    fn template(name: &str, content: &str) -> PromptTemplate {
        PromptTemplate {
            name: name.to_string(),
            description: String::new(),
            argument_hint: None,
            requires_arguments: false,
            content: content.to_string(),
            source_info: synthetic("/t.md"),
            file_path: "/t.md".to_string(),
        }
    }

    #[test]
    fn command_args_respect_single_and_double_quotes() {
        assert_eq!(parse_command_args("a b  c"), vec!["a", "b", "c"]);
        assert_eq!(parse_command_args("\"hello world\" second"), vec!["hello world", "second"]);
        assert_eq!(parse_command_args("'one two' three"), vec!["one two", "three"]);
        assert_eq!(parse_command_args(""), Vec::<String>::new());
        assert_eq!(parse_command_args("   "), Vec::<String>::new());
    }

    #[test]
    fn positional_arguments_substitute_and_missing_ones_are_empty() {
        let args: Vec<String> = vec!["one".to_string(), "two".to_string()];
        assert_eq!(substitute_args("$1-$2", &args), "one-two");
        assert_eq!(substitute_args("$3", &args), "");
        assert_eq!(substitute_args("$0", &args), "");
    }

    #[test]
    fn all_arguments_forms_substitute() {
        let args: Vec<String> = vec!["one".to_string(), "two".to_string()];
        assert_eq!(substitute_args("$@", &args), "one two");
        assert_eq!(substitute_args("$ARGUMENTS", &args), "one two");
    }

    #[test]
    fn defaulted_placeholders_fall_back_when_missing_or_empty() {
        let args: Vec<String> = vec!["one".to_string(), String::new()];
        assert_eq!(substitute_args("${1:-d}", &args), "one");
        assert_eq!(substitute_args("${2:-d}", &args), "d");
        assert_eq!(substitute_args("${9:-d}", &args), "d");
        assert_eq!(substitute_args("${@:-d}", &Vec::<String>::new()), "d");
    }

    #[test]
    fn slices_take_a_start_and_an_optional_length() {
        let args: Vec<String> = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(substitute_args("${@:2}", &args), "b c");
        assert_eq!(substitute_args("${@:2:1}", &args), "b");
        assert_eq!(substitute_args("${@:0}", &args), "a b c");
        assert_eq!(substitute_args("${@:9}", &args), "");
    }

    #[test]
    fn substitution_is_not_recursive() {
        let args: Vec<String> = vec!["$2".to_string()];
        assert_eq!(substitute_args("$1", &args), "$2");
    }

    #[test]
    fn a_template_file_uses_its_frontmatter_description() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("demo.md");
        std::fs::write(&path, "---\ndescription: from frontmatter\nargument-hint: <file>\n---\nbody text\n").expect("write");
        let (loaded, diagnostics) = load_template_from_file(&path.to_string_lossy(), synthetic("/x"));
        assert!(diagnostics.is_empty());
        let loaded = loaded.expect("template");
        assert_eq!(loaded.name, "demo");
        assert_eq!(loaded.description, "from frontmatter");
        assert_eq!(loaded.argument_hint.as_deref(), Some("<file>"));
        assert!(loaded.requires_arguments);
        assert_eq!(loaded.content, "body text");
    }

    #[test]
    fn a_template_without_frontmatter_takes_the_first_body_line_as_description() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bare.md");
        std::fs::write(&path, "\n\nfirst line here\nsecond\n").expect("write");
        let (loaded, _) = load_template_from_file(&path.to_string_lossy(), synthetic("/x"));
        let loaded = loaded.expect("template");
        assert_eq!(loaded.description, "first line here");
    }

    #[test]
    fn a_long_first_line_is_truncated_with_an_ellipsis() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("long.md");
        let line = "x".repeat(70);
        std::fs::write(&path, &line).expect("write");
        let (loaded, _) = load_template_from_file(&path.to_string_lossy(), synthetic("/x"));
        let loaded = loaded.expect("template");
        assert_eq!(loaded.description, format!("{}...", "x".repeat(60)));
    }

    #[test]
    fn a_missing_file_loads_as_none() {
        let (template, diagnostics) = load_template_from_file("/definitely/missing/template.md", synthetic("/x"));
        assert!(template.is_none());
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].diagnostic_type, ResourceDiagnosticType::Warning);
        assert_eq!(diagnostics[0].path.as_deref(), Some("/definitely/missing/template.md"));
    }

    #[test]
    fn explicit_requires_arguments_overrides_hint_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let true_path = dir.path().join("required.md");
        std::fs::write(&true_path, "---\nrequires-arguments: true\n---\nbody").expect("write");
        let (required, diagnostics) = load_template_from_file(&true_path.to_string_lossy(), synthetic("/x"));
        assert!(diagnostics.is_empty());
        assert!(required.expect("required template").requires_arguments);

        let false_path = dir.path().join("optional.md");
        std::fs::write(&false_path, "---\nargument-hint: <input>\nrequires-arguments: false\n---\nbody").expect("write");
        let (optional, diagnostics) = load_template_from_file(&false_path.to_string_lossy(), synthetic("/x"));
        assert!(diagnostics.is_empty());
        assert!(!optional.expect("optional template").requires_arguments);
    }

    #[test]
    fn a_template_without_hint_defaults_to_not_requiring_arguments() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("bare.md");
        std::fs::write(&path, "body").expect("write");
        let (template, diagnostics) = load_template_from_file(&path.to_string_lossy(), synthetic("/x"));
        assert!(diagnostics.is_empty());
        assert!(!template.expect("template").requires_arguments);
    }

    #[test]
    fn defaults_load_global_then_project_with_scoped_source_info() {
        let dir = tempfile::tempdir().expect("tempdir");
        let agent_dir = dir.path().join("agent");
        let cwd = dir.path().join("project");
        std::fs::create_dir_all(agent_dir.join("prompts")).expect("mkdir");
        std::fs::create_dir_all(cwd.join(config_dir_name()).join("prompts")).expect("mkdir");
        std::fs::write(agent_dir.join("prompts").join("user.md"), "user body").expect("write");
        std::fs::write(cwd.join(config_dir_name()).join("prompts").join("project.md"), "project body").expect("write");

        let (templates, diagnostics) = load_prompt_templates(&LoadPromptTemplatesOptions {
            cwd: cwd.to_string_lossy().into_owned(),
            agent_dir: agent_dir.to_string_lossy().into_owned(),
            prompt_paths: Vec::new(),
            include_defaults: true,
        });
        assert!(diagnostics.is_empty());
        let names: Vec<&str> = templates.iter().map(|template| template.name.as_str()).collect();
        assert_eq!(names, vec!["user", "project"]);
        assert_eq!(templates[0].source_info.scope, SourceScope::User);
        assert_eq!(templates[1].source_info.scope, SourceScope::Project);
    }

    #[test]
    fn explicit_paths_load_files_and_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cwd = dir.path().to_string_lossy().into_owned();
        let extra = dir.path().join("extra");
        std::fs::create_dir_all(&extra).expect("mkdir");
        std::fs::write(extra.join("a.md"), "a body").expect("write");
        let single = dir.path().join("single.md");
        std::fs::write(&single, "single body").expect("write");

        let (templates, diagnostics) = load_prompt_templates(&LoadPromptTemplatesOptions {
            cwd: cwd.clone(),
            agent_dir: dir.path().join("agent").to_string_lossy().into_owned(),
            prompt_paths: vec![
                extra.to_string_lossy().into_owned(),
                single.to_string_lossy().into_owned(),
                dir.path().join("missing.md").to_string_lossy().into_owned(),
            ],
            include_defaults: false,
        });
        assert!(diagnostics.is_empty());
        let names: Vec<&str> = templates.iter().map(|template| template.name.as_str()).collect();
        assert_eq!(names, vec!["a", "single"]);
        assert_eq!(templates[0].source_info.source, "local");
    }

    #[test]
    fn expansion_returns_the_original_text_when_nothing_matches() {
        let templates = vec![template("demo", "body $1")];
        assert_eq!(expand_prompt_template("plain text", &templates), "plain text");
        assert_eq!(expand_prompt_template("/missing", &templates), "/missing");
        assert_eq!(expand_prompt_template("/demo", &templates), "body ");
        assert_eq!(expand_prompt_template("/demo arg", &templates), "body arg");
        let expansion = expand_prompt_template_with_metadata("/demo arg", &templates);
        assert_eq!(expansion.template.map(|template| template.name), Some("demo".to_string()));
    }

    #[test]
    fn a_directory_scan_skips_non_markdown_and_broken_links() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("keep.md"), "body").expect("write");
        std::fs::write(dir.path().join("skip.txt"), "body").expect("write");
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.path().join("gone.md"), dir.path().join("broken.md")).expect("symlink");

        let (templates, diagnostics) = load_templates_from_dir(&dir.path().to_string_lossy(), &synthetic);
        assert!(diagnostics.is_empty());
        let names: Vec<&str> = templates.iter().map(|template| template.name.as_str()).collect();
        assert_eq!(names, vec!["keep"]);
    }

    #[test]
    fn malformed_frontmatter_reports_a_warning_without_losing_valid_templates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let invalid = dir.path().join("invalid.md");
        std::fs::write(&invalid, "---\ndescription: [unterminated\n---\nbody").expect("write");
        std::fs::write(dir.path().join("valid.md"), "valid body").expect("write");
        let (templates, diagnostics) = load_prompt_templates(&LoadPromptTemplatesOptions {
            cwd: dir.path().to_string_lossy().into_owned(),
            agent_dir: dir.path().join("agent").to_string_lossy().into_owned(),
            prompt_paths: vec![dir.path().to_string_lossy().into_owned()],
            include_defaults: false,
        });
        assert_eq!(templates.iter().map(|template| template.name.as_str()).collect::<Vec<_>>(), ["valid"]);
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].diagnostic_type, ResourceDiagnosticType::Warning);
        assert_eq!(diagnostics[0].path.as_deref(), Some(invalid.to_string_lossy().as_ref()));
    }
}
