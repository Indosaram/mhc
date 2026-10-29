//! Port of omo-senpi `components/git-master/skill-content.ts` at omo `7d818abaf9`: the additions
//! applied to the git-master skill text on read (the git-env-prefix section and prefixed bash
//! blocks, plus the optional attribution directive).
//!
//! Upstream imports `injectGitMasterConfig` from skills-loader-core; that template-injection
//! function has no native home yet, so the part senpi exercises (`commit_footer = false`, shell
//! `"unix"`) is ported here as [`inject_git_master_config`].

use crate::directive::{GitMasterSettings, build_git_master_attribution_directive};

/// Detect attribution in any original result text, not only the rewritten first item.
pub fn carries_attribution_directive<'a>(texts: impl IntoIterator<Item = &'a str>) -> bool {
    texts
        .into_iter()
        .any(|text| text.contains(crate::directive::COMMIT_ATTRIBUTION_OPEN_TAG))
}



/// Upstream `GitMasterSkillAdditions`.
pub struct GitMasterSkillAdditions {
    /// Upstream `rewriteSkillText`.
    pub rewrite_skill_text: Box<dyn Fn(&str) -> String + Send + Sync>,
    /// Upstream `directive`.
    pub directive: Option<String>,
}

/// Upstream `resolveGitMasterSkillAdditions`: `None` only when the prefix is empty and there is no
/// attribution directive.
///
/// Senpi's bash tool always runs bash (Git Bash on Windows) whatever the login shell is, so the
/// prefix is always the bash (`unix`) form.
pub fn resolve_git_master_skill_additions(
    settings: &GitMasterSettings,
) -> Option<GitMasterSkillAdditions> {
    let git_env_prefix = settings.git_env_prefix.clone();
    let directive = build_git_master_attribution_directive(settings, &git_env_prefix);
    if git_env_prefix.is_empty() && directive.is_none() {
        return None;
    }
    let rewrite_skill_text: Box<dyn Fn(&str) -> String + Send + Sync> = if git_env_prefix.is_empty()
    {
        Box::new(|text: &str| text.to_owned())
    } else {
        Box::new(move |text: &str| inject_after_frontmatter(text, &git_env_prefix))
    };
    Some(GitMasterSkillAdditions {
        rewrite_skill_text,
        directive,
    })
}

/// Upstream `FRONTMATTER_PATTERN` (`^---\r?\n[\s\S]*?\r?\n---\r?\n`): the byte length of the
/// leading frontmatter block, or 0 when the text has none.
fn frontmatter_len(text: &str) -> usize {
    let Some(rest) = text.strip_prefix("---") else {
        return 0;
    };
    let open_len = if rest.starts_with("\r\n") {
        2
    } else if rest.starts_with('\n') {
        1
    } else {
        return 0;
    };
    let body_start = 3 + open_len;
    let mut search_from = body_start;
    // Lazy `[\s\S]*?`: the earliest newline followed by a `---` fence line wins.
    while let Some(offset) = text[search_from..].find('\n') {
        let newline = search_from + offset;
        if let Some(after_fence) = text[newline + 1..].strip_prefix("---") {
            if after_fence.starts_with("\r\n") {
                return newline + 1 + 3 + 2;
            }
            if after_fence.starts_with('\n') {
                return newline + 1 + 3 + 1;
            }
        }
        search_from = newline + 1;
    }
    0
}

/// Upstream `injectAfterFrontmatter`.
fn inject_after_frontmatter(text: &str, git_env_prefix: &str) -> String {
    let (frontmatter, body) = text.split_at(frontmatter_len(text));
    format!(
        "{frontmatter}{}",
        inject_git_master_config(body, git_env_prefix)
    )
}

fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Upstream `isValidGitEnvPrefix`: empty, or space-separated `NAME=value` assignments where the
/// value is `[A-Za-z0-9_-]*`.
fn is_valid_git_env_prefix(value: &str) -> bool {
    value.is_empty()
        || value.split(' ').all(|pair| {
            pair.split_once('=').is_some_and(|(name, val)| {
                is_env_name(name)
                    && val
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            })
        })
}

/// Upstream `injectGitMasterConfig(template, { commit_footer: false, git_env_prefix }, "unix")`.
///
/// Upstream throws on an invalid prefix; the config schema already rejects those, so the native
/// port refuses to inject a shell-unsafe prefix and returns the template unchanged.
pub fn inject_git_master_config(template: &str, git_env_prefix: &str) -> String {
    if git_env_prefix.is_empty() || !is_valid_git_env_prefix(git_env_prefix) {
        return template.to_owned();
    }
    let result = inject_git_env_prefix(template, git_env_prefix);
    prefix_git_commands_in_bash_code_blocks(&result, git_env_prefix)
}

/// Upstream `injectGitEnvPrefix` with the `bash` code-block language.
fn inject_git_env_prefix(template: &str, prefix: &str) -> String {
    let section = [
        "## GIT COMMAND PREFIX (MANDATORY)".to_owned(),
        String::new(),
        "<git_env_prefix>".to_owned(),
        format!("**EVERY git command MUST be prefixed with `{prefix}`.**"),
        String::new(),
        "This allows custom git hooks to detect when git-master skill is active.".to_owned(),
        String::new(),
        "```bash".to_owned(),
        format!("{prefix} git status"),
        format!("{prefix} git add <files>"),
        format!("{prefix} git commit -m \"message\""),
        format!("{prefix} git push"),
        format!("{prefix} git rebase ..."),
        format!("{prefix} git log ..."),
        "```".to_owned(),
        String::new(),
        "**NO EXCEPTIONS. Every `git` invocation must include this prefix.**".to_owned(),
        "</git_env_prefix>".to_owned(),
    ]
    .join("\n");

    match template.find("## MODE DETECTION (FIRST STEP)") {
        Some(index) => format!(
            "{}{section}\n\n---\n\n{}",
            &template[..index],
            &template[index..]
        ),
        None => format!("{section}\n\n---\n\n{template}"),
    }
}

/// Upstream `prefixGitCommandsInBashCodeBlocks` (`/```bash\r?\n([\s\S]*?)```/g`).
fn prefix_git_commands_in_bash_code_blocks(template: &str, prefix: &str) -> String {
    const OPEN: &str = "```bash";
    let mut out = String::with_capacity(template.len());
    let mut cursor = 0;
    while let Some(found) = template[cursor..].find(OPEN) {
        let after_open = cursor + found + OPEN.len();
        let rest = &template[after_open..];
        let newline_len = if rest.starts_with("\r\n") {
            2
        } else if rest.starts_with('\n') {
            1
        } else {
            out.push_str(&template[cursor..after_open]);
            cursor = after_open;
            continue;
        };
        let code_start = after_open + newline_len;
        let Some(close) = template[code_start..].find("```") else {
            break;
        };
        let code_end = code_start + close;
        out.push_str(&template[cursor..code_start]);
        out.push_str(&prefix_git_commands_in_code_block(
            &template[code_start..code_end],
            prefix,
        ));
        cursor = code_end;
    }
    out.push_str(&template[cursor..]);
    out
}

/// Upstream `prefixGitCommandsInCodeBlock`.
fn prefix_git_commands_in_code_block(code_block: &str, prefix: &str) -> String {
    code_block
        .split('\n')
        .map(|line| {
            if line.contains(prefix) {
                line.to_owned()
            } else {
                prefix_inline_git(&prefix_leading_git(line, prefix), prefix)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_git_boundary(rest: &str) -> bool {
    matches!(rest.chars().next(), None | Some(' ' | '\t' | '\r'))
}

/// Upstream `LEADING_GIT_COMMAND_PATTERN` (`^([ \t]*(?:NAME=[^ \t]+\s+)*)git(?=[ \t]|$)`) on one line.
fn prefix_leading_git(line: &str, prefix: &str) -> String {
    let mut pos = line.len() - line.trim_start_matches([' ', '\t']).len();
    loop {
        let rest = &line[pos..];
        if let Some(after_git) = rest.strip_prefix("git")
            && is_git_boundary(after_git)
        {
            return format!("{}{prefix} {}", &line[..pos], &line[pos..]);
        }
        let token_end = rest.find([' ', '\t']).unwrap_or(rest.len());
        let is_assignment = rest[..token_end]
            .split_once('=')
            .is_some_and(|(name, value)| is_env_name(name) && !value.is_empty());
        let after_token = &rest[token_end..];
        let ws_len = after_token.len() - after_token.trim_start().len();
        if !is_assignment || ws_len == 0 {
            return line.to_owned();
        }
        pos += token_end + ws_len;
    }
}

/// Upstream `INLINE_GIT_COMMAND_PATTERN` (`([;&|()][ \t]*)git(?=[ \t]|$)`, global) on one line.
fn prefix_inline_git(line: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut cursor = 0;
    for (index, ch) in line.char_indices() {
        if index < cursor || !matches!(ch, ';' | '&' | '|' | '(' | ')') {
            continue;
        }
        let after_op = index + ch.len_utf8();
        let rest = &line[after_op..];
        let ws_len = rest.len() - rest.trim_start_matches([' ', '\t']).len();
        if let Some(after_git) = rest[ws_len..].strip_prefix("git")
            && is_git_boundary(after_git)
        {
            out.push_str(&line[cursor..after_op + ws_len]);
            out.push_str(prefix);
            out.push_str(" git");
            cursor = after_op + ws_len + 3;
        }
    }
    out.push_str(&line[cursor..]);
    out
}

#[cfg(test)]
mod attribution_tests {
    use super::carries_attribution_directive;

    #[test]
    fn detects_directive_in_later_result_items() {
        assert!(carries_attribution_directive([
            "skill",
            "<commit_attribution>existing"
        ]));
        assert!(!carries_attribution_directive(["skill", "ordinary text"]));
        assert!(!carries_attribution_directive(std::iter::empty()));
    }
}
