//! Skill-name frontmatter repair: SKILL.md files must carry a `name:` key;
//! missing or empty values are repaired from the skill directory name.
//! Port of `components/memory/commands/skill-frontmatter.ts` at pin 77f3067f1.

use std::path::{Path, PathBuf};

pub struct SkillNameFrontmatterRepairSkippedFile {
    pub path: String,
    pub reason: String,
}

#[derive(Default)]
pub struct SkillNameFrontmatterRepairResult {
    pub scanned: usize,
    pub repaired: Vec<String>,
    pub skipped: Vec<SkillNameFrontmatterRepairSkippedFile>,
}

pub struct SkillNameFrontmatterContentRepairResult {
    pub content: String,
    pub changed: bool,
    pub reason: Option<String>,
}

struct FrontmatterMatch {
    opening_len: usize,
    frontmatter_end: usize,
    closing_len: usize,
}

fn frontmatter_match(content: &str) -> Option<FrontmatterMatch> {
    let opening_len = if content.starts_with("---\r\n") {
        5
    } else if content.starts_with("---\n") {
        4
    } else {
        return None;
    };
    let mut search = opening_len;
    while let Some(relative) = content[search..].find("\n---") {
        let index = search + relative;
        let after = index + 4;
        let closing_len = if content[after..].starts_with("\r\n") {
            6
        } else if content[after..].starts_with('\n') {
            5
        } else if after == content.len() {
            4
        } else {
            search = index + 1;
            continue;
        };
        return Some(FrontmatterMatch {
            opening_len,
            frontmatter_end: index,
            closing_len,
        });
    }
    None
}

fn is_non_empty_name_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    let Some(rest) = trimmed.strip_prefix("name") else {
        return false;
    };
    let rest = rest.trim_start();
    let Some(rest) = rest.strip_prefix(':') else {
        return false;
    };
    !rest.trim().is_empty()
}

fn has_name_key(line: &str) -> bool {
    let trimmed = line.trim_start();
    let Some(rest) = trimmed.strip_prefix("name") else {
        return false;
    };
    rest.trim_start().starts_with(':')
}

fn format_yaml_scalar(value: &str) -> String {
    let plain = !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_' || character == '-');
    if plain {
        value.to_owned()
    } else {
        serde_json::to_string(value).unwrap_or_else(|_| value.to_owned())
    }
}

pub fn repair_skill_name_frontmatter_content(
    content: &str,
    skill_name: &str,
) -> SkillNameFrontmatterContentRepairResult {
    if skill_name.trim().is_empty() {
        return SkillNameFrontmatterContentRepairResult {
            content: content.to_owned(),
            changed: false,
            reason: Some("skill directory name is empty".to_owned()),
        };
    }

    let Some(found) = frontmatter_match(content) else {
        return SkillNameFrontmatterContentRepairResult {
            content: content.to_owned(),
            changed: false,
            reason: Some("missing YAML frontmatter".to_owned()),
        };
    };

    let opening = &content[..found.opening_len];
    let frontmatter = &content[found.opening_len..found.frontmatter_end];
    let closing = &content[found.frontmatter_end..found.frontmatter_end + found.closing_len];
    let newline = if opening.contains("\r\n") { "\r\n" } else { "\n" };
    let normalized = frontmatter.replace("\r\n", "\n");
    let mut lines: Vec<String> = normalized.split('\n').map(str::to_owned).collect();
    let name_line_index = lines.iter().position(|line| has_name_key(line));

    if let Some(index) = name_line_index
        && is_non_empty_name_line(&lines[index])
    {
        return SkillNameFrontmatterContentRepairResult {
            content: content.to_owned(),
            changed: false,
            reason: None,
        };
    }

    let name_line = format!("name: {}", format_yaml_scalar(skill_name.trim()));
    match name_line_index {
        Some(index) => lines[index] = name_line,
        None => lines.insert(0, name_line),
    }

    let match_len = found.opening_len + frontmatter.len() + found.closing_len;
    let next = format!(
        "{opening}{}{closing}{}",
        lines.join(newline),
        &content[match_len..]
    );
    SkillNameFrontmatterContentRepairResult { content: next, changed: true, reason: None }
}

fn path_exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

fn find_skill_markdown_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == ".git" || name == "node_modules" {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            files.extend(find_skill_markdown_files(&entry.path())?);
        } else if file_type.is_file() && name == "SKILL.md" {
            files.push(entry.path());
        }
    }
    Ok(files)
}

pub fn repair_missing_skill_name_frontmatter(
    memory_dir: Option<&Path>,
) -> SkillNameFrontmatterRepairResult {
    let mut result = SkillNameFrontmatterRepairResult::default();
    let Some(memory_dir) = memory_dir else {
        return result;
    };

    let skills_dir = memory_dir.join("skills");
    if !path_exists(&skills_dir) {
        return result;
    }

    let mut skill_files = match find_skill_markdown_files(&skills_dir) {
        Ok(files) => files,
        Err(error) => {
            result.skipped.push(SkillNameFrontmatterRepairSkippedFile {
                path: "skills/".to_owned(),
                reason: format!("failed to scan skills directory: {error}"),
            });
            return result;
        }
    };
    skill_files.sort();

    for skill_file in skill_files {
        let display_path = relative_display_path(memory_dir, &skill_file);
        result.scanned += 1;
        let content = match std::fs::read_to_string(&skill_file) {
            Ok(content) => content,
            Err(error) => {
                result.skipped.push(SkillNameFrontmatterRepairSkippedFile {
                    path: display_path,
                    reason: error.to_string(),
                });
                continue;
            }
        };
        let skill_name = skill_file
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned();
        let repair = repair_skill_name_frontmatter_content(&content, &skill_name);
        if let Some(reason) = repair.reason {
            result
                .skipped
                .push(SkillNameFrontmatterRepairSkippedFile { path: display_path, reason });
            continue;
        }
        if !repair.changed {
            continue;
        }
        if let Err(error) = std::fs::write(&skill_file, repair.content) {
            result
                .skipped
                .push(SkillNameFrontmatterRepairSkippedFile { path: display_path, reason: error.to_string() });
            continue;
        }
        result.repaired.push(display_path);
    }
    result
}

fn relative_display_path(memory_dir: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(memory_dir).unwrap_or(path);
    relative.to_string_lossy().replace('\\', "/")
}

pub fn format_skill_name_frontmatter_repair_report(
    result: &SkillNameFrontmatterRepairResult,
) -> String {
    let mut sections: Vec<String> = Vec::new();
    if !result.repaired.is_empty() {
        sections.push(format!(
            "added missing `name:` frontmatter to {} skill{}: {}",
            result.repaired.len(),
            if result.repaired.len() == 1 { "" } else { "s" },
            result
                .repaired
                .iter()
                .map(|path| format!("`{path}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !result.skipped.is_empty() {
        sections.push(format!(
            "could not repair {} skill{}: {}",
            result.skipped.len(),
            if result.skipped.len() == 1 { "" } else { "s" },
            result
                .skipped
                .iter()
                .map(|item| format!("`{}` ({})", item.path, item.reason))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    sections.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp root")
    }

    #[test]
    fn given_frontmatter_without_a_name_key_when_repaired_then_the_directory_name_is_inserted_first() {
        let content = "---\ndescription: Commit helper\n---\nBody\n";
        let result = repair_skill_name_frontmatter_content(content, "commit");
        assert!(result.changed);
        assert_eq!(result.content, "---\nname: commit\ndescription: Commit helper\n---\nBody\n");
    }

    #[test]
    fn given_an_existing_non_empty_name_when_repaired_then_the_content_is_untouched() {
        let content = "---\nname: commit\ndescription: Commit helper\n---\n";
        let result = repair_skill_name_frontmatter_content(content, "other");
        assert!(!result.changed);
        assert_eq!(result.content, content);
    }

    #[test]
    fn given_an_empty_name_value_when_repaired_then_the_line_is_replaced_in_place() {
        let content = "---\ndescription: x\nname:   \n---\n";
        let result = repair_skill_name_frontmatter_content(content, "commit");
        assert!(result.changed);
        assert_eq!(result.content, "---\ndescription: x\nname: commit\n---\n");
    }

    #[test]
    fn given_no_frontmatter_at_all_when_repaired_then_the_file_is_skipped_with_a_reason() {
        let result = repair_skill_name_frontmatter_content("# just markdown\n", "commit");
        assert!(!result.changed);
        assert_eq!(result.reason.as_deref(), Some("missing YAML frontmatter"));
    }

    #[test]
    fn given_a_name_needing_quoting_when_repaired_then_the_scalar_is_quoted() {
        let content = "---\ndescription: x\n---\n";
        let result = repair_skill_name_frontmatter_content(content, "my skill");
        assert!(result.content.contains("name: \"my skill\""));
    }

    #[test]
    fn given_no_skills_directory_when_repaired_then_nothing_is_scanned() {
        let dir = temp_root();
        let result = repair_missing_skill_name_frontmatter(Some(dir.path()));
        assert_eq!(result.scanned, 0);
        assert!(result.repaired.is_empty());
        assert!(result.skipped.is_empty());
    }

    #[test]
    fn given_skill_files_in_nested_directories_when_repaired_then_only_name_less_files_change() {
        let dir = temp_root();
        std::fs::create_dir_all(dir.path().join("skills").join("commit")).expect("commit");
        std::fs::write(dir.path().join("skills").join("commit").join("SKILL.md"), "---\ndescription: x\n---\n")
            .expect("commit skill");
        std::fs::create_dir_all(dir.path().join("skills").join("nested").join("review")).expect("nested");
        std::fs::write(
            dir.path().join("skills").join("nested").join("review").join("SKILL.md"),
            "---\nname: review\ndescription: y\n---\n",
        )
        .expect("review skill");
        std::fs::create_dir_all(dir.path().join("skills").join("flat")).expect("flat");
        std::fs::write(dir.path().join("skills").join("flat").join("notes.md"), "---\ndescription: not a skill\n---\n")
            .expect("notes");

        let result = repair_missing_skill_name_frontmatter(Some(dir.path()));

        assert_eq!(result.scanned, 2);
        assert_eq!(result.repaired, vec!["skills/commit/SKILL.md".to_owned()]);
        assert!(result.skipped.is_empty());
        let repaired = std::fs::read_to_string(dir.path().join("skills").join("commit").join("SKILL.md"))
            .expect("repaired");
        assert!(repaired.contains("name: commit"));
    }

    #[test]
    fn given_a_repaired_file_and_a_skipped_file_when_the_report_is_formatted_then_both_sections_render() {
        let dir = temp_root();
        std::fs::create_dir_all(dir.path().join("skills").join("commit")).expect("commit");
        std::fs::write(dir.path().join("skills").join("commit").join("SKILL.md"), "---\ndescription: x\n---\n")
            .expect("commit skill");
        std::fs::create_dir_all(dir.path().join("skills").join("broken")).expect("broken");
        std::fs::write(dir.path().join("skills").join("broken").join("SKILL.md"), "no frontmatter\n")
            .expect("broken skill");
        let result = repair_missing_skill_name_frontmatter(Some(dir.path()));
        let report = format_skill_name_frontmatter_repair_report(&result);
        assert!(report.contains("skills/commit/SKILL.md"));
        assert!(report.contains("skills/broken/SKILL.md"));
        assert!(report.contains("missing YAML frontmatter"));
    }
}
