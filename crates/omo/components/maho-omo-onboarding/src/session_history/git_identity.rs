//! Git identity read straight from `.git` (upstream `git-identity.ts`).

use super::project_paths::resolve_path;
use super::session_history_fs::{
    FileKind, SessionHistoryFs, read_file_string_or_empty, stat_or_null,
};
use super::transcript_discovery::join;
use super::types::ProjectGit;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitIdentity {
    Repository(ProjectGit),
    Worktree,
    NotGit,
}

fn azure_dev_ops_repository_key(host: &str, segments: &[&str]) -> Option<String> {
    if host != "ssh.dev.azure.com" && host != "vs-ssh.visualstudio.com" {
        return None;
    }
    let [marker, organization, project, repository] = segments else {
        return None;
    };
    if *marker != "v3" || organization.is_empty() || project.is_empty() || repository.is_empty() {
        return None;
    }
    Some(if host == "ssh.dev.azure.com" {
        format!("dev.azure.com/{organization}/{project}/_git/{repository}")
    } else {
        format!("{organization}.visualstudio.com/{project}/_git/{repository}")
    })
}

fn has_url_scheme(value: &str) -> bool {
    ["ssh://", "http://", "https://", "git://"]
        .iter()
        .any(|scheme| value.starts_with(scheme))
}

pub fn normalize_git_remote_url(value: &str) -> String {
    let mut normalized = value.trim().trim_end_matches('/').to_lowercase();
    if normalized.ends_with(".git") {
        normalized.truncate(normalized.len() - 4);
    }
    if has_url_scheme(&normalized) {
        let rest = &normalized[normalized.find("://").map_or(0, |at| at + 3)..];
        let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
        let path = path.split(['?', '#']).next().unwrap_or("");
        let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let host = if host_port.starts_with('[') {
            host_port
                .split_once(']')
                .map_or(host_port, |(h, _)| &host_port[..=h.len()])
        } else {
            host_port.split(':').next().unwrap_or(host_port)
        };
        let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if !host.is_empty() && segments.len() > 1 {
            return azure_dev_ops_repository_key(host, &segments)
                .unwrap_or_else(|| format!("{host}/{}", segments.join("/")));
        }
        return normalized;
    }
    if let Some((user, rest)) = normalized.split_once('@')
        && !user.is_empty()
        && user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && let Some((host, path)) = rest.split_once(':')
        && !host.is_empty()
        && !host.contains(['/', ' ', '\t'])
        && !path.contains(char::is_whitespace)
    {
        let segments: Vec<&str> = path.split('/').collect();
        if segments.len() >= 2 && segments.iter().all(|s| !s.is_empty()) {
            return azure_dev_ops_repository_key(host, &segments)
                .unwrap_or_else(|| format!("{host}/{path}"));
        }
    }
    normalized
}

fn parse_git_config_value(raw: &str) -> String {
    let chars: Vec<char> = raw.chars().collect();
    let mut out = String::new();
    let mut quoted = false;
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if c == '\\' && index + 1 < chars.len() {
            out.push(chars[index + 1]);
            index += 2;
            continue;
        }
        if c == '"' {
            quoted = !quoted;
        } else if !quoted && (c == '#' || c == ';') {
            break;
        } else {
            out.push(c);
        }
        index += 1;
    }
    out.trim().to_owned()
}

/// `[remote "name"]` or `[remote.name]` header; returns the section name.
fn remote_section(line: &str) -> Option<String> {
    let inner = line.strip_prefix('[')?.trim_start();
    if !inner.get(..6)?.eq_ignore_ascii_case("remote") {
        return None;
    }
    let rest = inner.get(6..)?;
    let (name, after) = if let Some(dotted) = rest.strip_prefix('.') {
        let end = dotted.find(|c: char| c == ']' || c.is_whitespace())?;
        if end == 0 {
            return None;
        }
        (dotted[..end].to_lowercase(), &dotted[end..])
    } else {
        let spaced = rest.trim_start();
        if spaced.len() == rest.len() {
            return None;
        }
        let quoted = spaced.strip_prefix('"')?;
        let end = quoted.find('"')?;
        if end == 0 {
            return None;
        }
        (quoted[..end].to_owned(), &quoted[end + 1..])
    };
    let after = after.trim_start().strip_prefix(']')?.trim_start();
    (after.is_empty() || after.starts_with('#') || after.starts_with(';')).then_some(name)
}

/// Unfold backslash-newline continuations (`\\\r?\n[ \t]*`).
fn join_continuations(text: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '\\' {
            let mut next = index + 1;
            if chars.get(next) == Some(&'\r') {
                next += 1;
            }
            if chars.get(next) == Some(&'\n') {
                index = next + 1;
                while matches!(chars.get(index), Some(' ' | '\t')) {
                    index += 1;
                }
                continue;
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

/// `remote.origin.url` from raw `.git/config` text, falling back to the first remote.
pub fn parse_origin_url_from_git_config(config_text: &str) -> Option<String> {
    let mut section: Option<String> = None;
    let mut origin_url: Option<String> = None;
    let mut first_remote_url: Option<String> = None;
    for raw_line in join_continuations(config_text).split('\n') {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = remote_section(line) {
            section = Some(name);
            continue;
        }
        if line.starts_with('[') {
            section = None;
            continue;
        }
        let Some(current) = &section else { continue };
        if !line
            .get(..3)
            .is_some_and(|head| head.eq_ignore_ascii_case("url"))
        {
            continue;
        }
        let Some(value) = line
            .get(3..)
            .and_then(|tail| tail.trim_start().strip_prefix('='))
        else {
            continue;
        };
        let url = parse_git_config_value(value.trim_start());
        if url.is_empty() {
            continue;
        }
        if current == "origin" {
            origin_url.get_or_insert(url);
        } else {
            first_remote_url.get_or_insert(url);
        }
    }
    origin_url.or(first_remote_url)
}

pub fn parse_github_repository_name_with_owner_from_remote_url(
    url: Option<&str>,
) -> Option<String> {
    let trimmed = url?.trim();
    let lower = trimmed.to_lowercase();
    let prefixes = [
        "git@github.com:",
        "ssh://git@github.com/",
        "ssh://github.com/",
        "https://github.com/",
        "git://github.com/",
    ];
    let prefix = prefixes.iter().find(|p| lower.starts_with(**p))?;
    let rest = &trimmed[prefix.len()..];
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let (owner, repo) = rest.split_once('/')?;
    let repo_trimmed = match repo.len().checked_sub(4).and_then(|at| repo.get(at..)) {
        Some(tail) if tail.eq_ignore_ascii_case(".git") && repo.len() > 4 => {
            &repo[..repo.len() - 4]
        }
        _ => repo,
    };
    let clean =
        |part: &str| !part.is_empty() && !part.contains('/') && !part.contains(char::is_whitespace);
    (clean(owner) && clean(repo_trimmed)).then(|| format!("{owner}/{repo_trimmed}"))
}

fn gitdir_target(pointer: &str) -> Option<String> {
    pointer.split('\n').find_map(|line| {
        let rest = line.trim_end_matches('\r').strip_prefix("gitdir:")?;
        let target = rest.trim();
        (!target.is_empty()).then(|| target.to_owned())
    })
}

fn is_linked_worktree_dir(git_dir: &str) -> bool {
    let normalized = git_dir.replace('\\', "/");
    let trimmed = normalized.trim_end_matches('/');
    let mut parts = trimmed.rsplit('/');
    parts.next().is_some_and(|last| !last.is_empty()) && parts.next() == Some("worktrees")
}

/// Reads `.git` directly instead of spawning git. A `.git` file pointing into `worktrees/` is a linked
/// worktree, whose history belongs to the main checkout; submodules (`modules/`) count as repositories.
pub fn read_git_identity(fs: &dyn SessionHistoryFs, directory: &str) -> GitIdentity {
    let git_path = join(directory, ".git");
    let Some(stats) = stat_or_null(fs, &git_path) else {
        return GitIdentity::NotGit;
    };
    let mut git_dir = git_path.clone();
    if stats.kind != FileKind::Directory {
        let pointer = read_file_string_or_empty(fs, &git_path);
        let Some(target) = gitdir_target(&pointer) else {
            return GitIdentity::NotGit;
        };
        git_dir = resolve_path(&join(directory, &target));
        if is_linked_worktree_dir(&git_dir) {
            return GitIdentity::Worktree;
        }
    }
    let origin =
        parse_origin_url_from_git_config(&read_file_string_or_empty(fs, &join(&git_dir, "config")));
    GitIdentity::Repository(ProjectGit {
        remote_key: origin.as_deref().map(normalize_git_remote_url),
        repository: parse_github_repository_name_with_owner_from_remote_url(origin.as_deref()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_history::session_history_fs::NodeSessionHistoryFs;

    #[test]
    fn normalizes_remote_urls() {
        assert_eq!(
            normalize_git_remote_url("https://github.com/Owner/Repo.git"),
            "github.com/owner/repo"
        );
        assert_eq!(
            normalize_git_remote_url("git@github.com:Owner/Repo.git"),
            "github.com/owner/repo"
        );
        assert_eq!(
            normalize_git_remote_url("ssh://git@host:2222/a/b/"),
            "host/a/b"
        );
        assert_eq!(
            normalize_git_remote_url("git@ssh.dev.azure.com:v3/Org/Proj/Repo"),
            "dev.azure.com/org/proj/_git/repo"
        );
        assert_eq!(
            normalize_git_remote_url("org@vs-ssh.visualstudio.com:v3/Org/Proj/Repo"),
            "org.visualstudio.com/proj/_git/repo"
        );
        assert_eq!(normalize_git_remote_url("/local/path/"), "/local/path");
    }

    #[test]
    fn parses_origin_with_fallback_comments_and_continuations() {
        let config = "[core]\n\turl = nope\n[remote \"upstream\"]\n\turl = https://x/up.git\n[remote \"origin\"]\n\turl = \"https://x/o\" # c\n";
        assert_eq!(
            parse_origin_url_from_git_config(config).as_deref(),
            Some("https://x/o")
        );
        let only = "[remote.fork]\nurl = https://x/fork\n";
        assert_eq!(
            parse_origin_url_from_git_config(only).as_deref(),
            Some("https://x/fork")
        );
        let continued = "[remote \"origin\"]\nurl = https://x/\\\n   long\n";
        assert_eq!(
            parse_origin_url_from_git_config(continued).as_deref(),
            Some("https://x/long")
        );
        assert_eq!(parse_origin_url_from_git_config("[core]\n"), None);
    }

    #[test]
    fn extracts_github_names() {
        let parse = |url: &str| parse_github_repository_name_with_owner_from_remote_url(Some(url));
        assert_eq!(parse("git@github.com:o/r.git").as_deref(), Some("o/r"));
        assert_eq!(parse("https://github.com/o/r/").as_deref(), Some("o/r"));
        assert_eq!(parse("https://gitlab.com/o/r"), None);
        assert_eq!(
            parse_github_repository_name_with_owner_from_remote_url(None),
            None
        );
    }

    #[test]
    fn reads_directory_pointer_and_worktree_identities() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git"))?;
        std::fs::write(
            repo.join(".git/config"),
            "[remote \"origin\"]\nurl = git@github.com:o/r.git\n",
        )?;
        let identity = read_git_identity(&NodeSessionHistoryFs, &repo.to_string_lossy());
        assert_eq!(
            identity,
            GitIdentity::Repository(ProjectGit {
                remote_key: Some("github.com/o/r".into()),
                repository: Some("o/r".into())
            })
        );
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(&wt)?;
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}/.git/worktrees/wt\n", repo.display()),
        )?;
        assert_eq!(
            read_git_identity(&NodeSessionHistoryFs, &wt.to_string_lossy()),
            GitIdentity::Worktree
        );
        assert_eq!(
            read_git_identity(&NodeSessionHistoryFs, &dir.path().to_string_lossy()),
            GitIdentity::NotGit
        );
        Ok(())
    }
}
