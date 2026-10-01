//! Port of senpi packages/agent/src/harness/skills.ts.

use super::context::Context;
use super::types::{ExecutionEnv, FileError, FileErrorCode, FileInfo, FileKind, Skill};
use ignore::gitignore::GitignoreBuilder;
use maho_ai::types::BoxFuture;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDiagnostic {
    pub r#type: &'static str,
    pub code: &'static str,
    pub message: String,
    pub path: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct LoadedSkills {
    pub skills: Vec<Skill>,
    pub diagnostics: Vec<SkillDiagnostic>,
}

pub fn format_skill_invocation(skill: &Skill, additional_instructions: Option<&str>) -> String {
    let normalized = skill.file_path.trim_end_matches(['/', '\\']);
    let separator = normalized.rfind(['/', '\\']).unwrap_or(0);
    let dirname = if separator == 2 && normalized.as_bytes().get(1) == Some(&b':') {
        &normalized[..3]
    } else if separator == 0 {
        "/"
    } else {
        &normalized[..separator]
    };
    let block = format!(
        "<skill name=\"{}\" location=\"{}\">\nReferences are relative to {dirname}.\n\n{}\n</skill>",
        skill.name, skill.file_path, skill.content
    );
    match additional_instructions.filter(|s| !s.is_empty()) {
        Some(extra) => format!("{block}\n\n{extra}"),
        None => block,
    }
}

fn diagnostic(out: &mut LoadedSkills, code: &'static str, message: impl Into<String>, path: &str) {
    out.diagnostics.push(SkillDiagnostic {
        r#type: "warning",
        code,
        message: message.into(),
        path: path.into(),
    });
}

fn info_error(out: &mut LoadedSkills, error: FileError, path: &str) {
    if error.code != FileErrorCode::NotFound {
        diagnostic(out, "file_info_failed", error.message, path);
    }
}

async fn resolve_kind(
    env: &dyn ExecutionEnv,
    info: &FileInfo,
    out: &mut LoadedSkills,
    context: &Context,
) -> Option<FileKind> {
    if info.kind != FileKind::Symlink {
        return Some(info.kind);
    }
    let path = match env.canonical_path(&info.path, context).await {
        Ok(path) => path,
        Err(error) => {
            info_error(out, error, &info.path);
            return None;
        }
    };
    match env.file_info(&path, context).await {
        Ok(target) => match target.kind {
            FileKind::File | FileKind::Directory => Some(target.kind),
            FileKind::Symlink => None,
        },
        Err(error) => {
            info_error(out, error, &info.path);
            None
        }
    }
}

pub async fn load_skills(env: &dyn ExecutionEnv, dirs: &[&str], context: &Context) -> LoadedSkills {
    let mut out = LoadedSkills::default();
    for dir in dirs {
        let info = match env.file_info(dir, context).await {
            Ok(info) => info,
            Err(error) => {
                info_error(&mut out, error, dir);
                continue;
            }
        };
        if resolve_kind(env, &info, &mut out, context).await != Some(FileKind::Directory) {
            continue;
        }
        let mut rules = GitignoreBuilder::new(&info.path);
        load_dir(
            env, &info.path, true, &mut rules, &info.path, context, &mut out,
        )
        .await;
    }
    out
}

#[derive(Debug, PartialEq, Eq)]
pub struct SourcedSkill<TSource, TSkill = Skill> {
    pub skill: TSkill,
    pub source: TSource,
}
#[derive(Debug, PartialEq, Eq)]
pub struct SourcedDiagnostic<TSource> {
    pub diagnostic: SkillDiagnostic,
    pub source: TSource,
}
#[derive(Debug, PartialEq, Eq)]
pub struct LoadedSourcedSkills<TSource, TSkill = Skill> {
    pub skills: Vec<SourcedSkill<TSource, TSkill>>,
    pub diagnostics: Vec<SourcedDiagnostic<TSource>>,
}
pub async fn load_sourced_skills<TSource: Clone, TSkill>(
    env: &dyn ExecutionEnv,
    inputs: &[(&str, TSource)],
    map_skill: impl Fn(Skill, &TSource, &Context) -> TSkill,
    context: &Context,
) -> LoadedSourcedSkills<TSource, TSkill> {
    let mut out = LoadedSourcedSkills {
        skills: Vec::new(),
        diagnostics: Vec::new(),
    };
    for (path, source) in inputs {
        let loaded = load_skills(env, &[*path], context).await;
        out.skills
            .extend(loaded.skills.into_iter().map(|skill| SourcedSkill {
                skill: map_skill(skill, source, context),
                source: source.clone(),
            }));
        out.diagnostics
            .extend(
                loaded
                    .diagnostics
                    .into_iter()
                    .map(|diagnostic| SourcedDiagnostic {
                        diagnostic,
                        source: source.clone(),
                    }),
            );
    }
    out
}

fn relative_env_path(root: &str, path: &str) -> String {
    let root = root.replace('\\', "/");
    let path = path.replace('\\', "/");
    let root = root.trim_end_matches('/');
    let path = path.trim_end_matches('/');
    if path == root {
        String::new()
    } else {
        path.strip_prefix(&format!("{root}/"))
            .unwrap_or_else(|| path.trim_start_matches('/'))
            .into()
    }
}

fn load_dir<'a>(
    env: &'a dyn ExecutionEnv,
    dir: &'a str,
    include_root: bool,
    rules: &'a mut GitignoreBuilder,
    root: &'a str,
    context: &'a Context,
    out: &'a mut LoadedSkills,
) -> BoxFuture<'a, ()> {
    Box::pin(async move {
        let info = match env.file_info(dir, context).await {
            Ok(info) => info,
            Err(error) => {
                info_error(out, error, dir);
                return;
            }
        };
        if resolve_kind(env, &info, out, context).await != Some(FileKind::Directory) {
            return;
        }
        let relative = relative_env_path(root, dir);
        let prefix = if relative.is_empty() {
            String::new()
        } else {
            format!("{relative}/")
        };
        for filename in [".gitignore", ".ignore", ".fdignore"] {
            let path = match env
                .join_path(vec![dir.into(), filename.into()], context)
                .await
            {
                Ok(path) => path,
                Err(error) => {
                    diagnostic(out, "file_info_failed", error.message, dir);
                    continue;
                }
            };
            match env.file_info(&path, context).await {
                Ok(info) if info.kind == FileKind::File => {}
                Ok(_) => continue,
                Err(error) => {
                    info_error(out, error, &path);
                    continue;
                }
            }
            let content = match env.read_text_file(&path, context).await {
                Ok(content) => content,
                Err(error) => {
                    diagnostic(out, "read_failed", error.message, &path);
                    continue;
                }
            };
            for line in content.lines() {
                if line.trim().is_empty() || line.trim().starts_with('#') {
                    continue;
                }
                let (negated, pattern) = if let Some(rest) = line.strip_prefix('!') {
                    ("!", rest)
                } else {
                    ("", line.strip_prefix("\\!").map_or(line, |_| &line[1..]))
                };
                let pattern = format!("{negated}{prefix}{}", pattern.trim_start_matches('/'));
                if let Err(error) = rules.add_line(None, &pattern) {
                    diagnostic(out, "parse_failed", error.to_string(), &path);
                }
            }
        }
        let mut entries = match env.list_dir(dir, context).await {
            Ok(entries) => entries,
            Err(error) => {
                diagnostic(out, "list_failed", error.message, dir);
                return;
            }
        };
        let matcher = match rules.build() {
            Ok(matcher) => matcher,
            Err(error) => {
                diagnostic(out, "parse_failed", error.to_string(), dir);
                return;
            }
        };
        for entry in entries.iter().filter(|entry| entry.name == "SKILL.md") {
            if resolve_kind(env, entry, out, context).await != Some(FileKind::File) {
                continue;
            }
            if matcher
                .matched_path_or_any_parents(&entry.path, false)
                .is_ignore()
            {
                continue;
            }
            load_file(env, &entry.path, &info.name, context, out).await;
            return;
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        for entry in entries {
            if entry.name.starts_with('.') || entry.name == "node_modules" {
                continue;
            }
            let Some(kind) = resolve_kind(env, &entry, out, context).await else {
                continue;
            };
            // Rebuild because nested rules accumulate in the original shared matcher.
            let matcher = match rules.build() {
                Ok(matcher) => matcher,
                Err(error) => {
                    diagnostic(out, "parse_failed", error.to_string(), dir);
                    return;
                }
            };
            if matcher
                .matched_path_or_any_parents(&entry.path, kind == FileKind::Directory)
                .is_ignore()
            {
                continue;
            }
            if kind == FileKind::Directory {
                load_dir(env, &entry.path, false, rules, root, context, out).await;
            } else if kind == FileKind::File && include_root && entry.name.ends_with(".md") {
                load_file(env, &entry.path, &info.name, context, out).await;
            }
        }
    })
}

async fn load_file(
    env: &dyn ExecutionEnv,
    path: &str,
    parent: &str,
    context: &Context,
    out: &mut LoadedSkills,
) {
    let declared = path
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        == Some("SKILL.md");
    let raw = match env.read_text_file(path, context).await {
        Ok(raw) => raw,
        Err(error) => {
            diagnostic(out, "read_failed", error.message, path);
            return;
        }
    };
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let mut body = normalized.as_str();
    let mut metadata = serde_yaml::Value::Null;
    if normalized.starts_with("---")
        && let Some(end) = normalized[3..].find("\n---").map(|n| n + 3)
    {
        match serde_yaml::from_str::<serde_yaml::Value>(normalized.get(4..end).unwrap_or("")) {
            Ok(value) => metadata = value,
            Err(error) => {
                if declared {
                    diagnostic(out, "parse_failed", error.to_string(), path);
                }
                return;
            }
        }
        body = normalized[end + 4..].trim();
    }
    let description = metadata
        .get("description")
        .and_then(serde_yaml::Value::as_str)
        .unwrap_or("");
    if !declared && description.trim().is_empty() {
        return;
    }
    if description.trim().is_empty() {
        diagnostic(out, "invalid_metadata", "description is required", path);
    } else if description.encode_utf16().count() > 1024 {
        diagnostic(
            out,
            "invalid_metadata",
            format!(
                "description exceeds 1024 characters ({})",
                description.encode_utf16().count()
            ),
            path,
        );
    }
    let name = metadata
        .get("name")
        .and_then(serde_yaml::Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(parent);
    if name != parent {
        diagnostic(
            out,
            "invalid_metadata",
            format!("name \"{name}\" does not match parent directory \"{parent}\""),
            path,
        );
    }
    if name.encode_utf16().count() > 64 {
        diagnostic(
            out,
            "invalid_metadata",
            format!(
                "name exceeds 64 characters ({})",
                name.encode_utf16().count()
            ),
            path,
        );
    }
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        diagnostic(
            out,
            "invalid_metadata",
            "name contains invalid characters (must be lowercase a-z, 0-9, hyphens only)",
            path,
        );
    }
    if name.starts_with('-') || name.ends_with('-') {
        diagnostic(
            out,
            "invalid_metadata",
            "name must not start or end with a hyphen",
            path,
        );
    }
    if name.contains("--") {
        diagnostic(
            out,
            "invalid_metadata",
            "name must not contain consecutive hyphens",
            path,
        );
    }
    if description.trim().is_empty() {
        return;
    }
    out.skills.push(Skill {
        name: name.into(),
        description: description.into(),
        content: body.into(),
        file_path: path.into(),
        disable_model_invocation: Some(
            metadata
                .get("disable-model-invocation")
                .and_then(serde_yaml::Value::as_bool)
                == Some(true),
        ),
    });
}
