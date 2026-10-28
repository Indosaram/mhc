//! Git repository identity used by session moves (senpi core/repository-identity.ts).
use crate::session_move::bundle::RepositoryIdentity;
async fn git(dir: &str, args: &[&str]) -> Option<String> {
    let mut command = tokio::process::Command::new("git");
    command.arg("-C").arg(dir).args(args).kill_on_drop(true);
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
    ] {
        command.env_remove(name);
    }
    let output = tokio::time::timeout(std::time::Duration::from_secs(5), command.output())
        .await
        .ok()?
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}
pub fn normalize_remote_url(url: &str) -> String {
    let mut value = url.trim().to_owned();
    if let Some((_, tail)) = value.split_once("://") {
        value = tail.into();
    }
    if let Some((_, tail)) = value.split_once('@') {
        value = tail.into();
    }
    if let Some((host, path)) = value.split_once(':')
        && !path.starts_with('/') {
            if let Some((port, rest)) = path.split_once('/') {
                if port.chars().all(|ch| ch.is_ascii_digit()) {
                    value = format!("{host}/{rest}");
                } else {
                    value = format!("{host}/{path}");
                }
            } else {
                value = format!("{host}/{path}");
            }
        }
    let mut value = value.trim_end_matches('/').to_lowercase();
    if value.ends_with(".git") {
        value.truncate(value.len() - 4);
    }
    value
}
pub async fn read_repository_identity(dir: &str) -> Option<RepositoryIdentity> {
    let (roots, origin) = tokio::join!(
        git(dir, &["rev-list", "--max-parents=0", "HEAD"]),
        git(dir, &["config", "--get", "remote.origin.url"])
    );
    let mut root_commits = roots
        .unwrap_or_default()
        .split_whitespace()
        .filter(|line| {
            (40..=64).contains(&line.len())
                && line
                    .chars()
                    .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    root_commits.sort();
    let origin_url = origin
        .filter(|origin| !origin.trim().is_empty())
        .map(|origin| normalize_remote_url(&origin));
    if root_commits.is_empty() && origin_url.is_none() {
        None
    } else {
        Some(RepositoryIdentity {
            root_commits,
            origin_url,
        })
    }
}
pub fn compare_repository_identities(
    a: Option<&RepositoryIdentity>,
    b: Option<&RepositoryIdentity>,
) -> &'static str {
    let (Some(a), Some(b)) = (a, b) else {
        return "unknown";
    };
    if !a.root_commits.is_empty() && !b.root_commits.is_empty() {
        if a.root_commits
            .iter()
            .any(|root| b.root_commits.contains(root))
        {
            "same"
        } else {
            "different"
        }
    } else if let (Some(a), Some(b)) = (&a.origin_url, &b.origin_url) {
        if a == b { "same" } else { "different" }
    } else {
        "unknown"
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_transport_does_not_change_identity() {
        for remote in [
            "git@Host:owner/repo.git",
            "ssh://git@Host:22/owner/repo",
            "https://Host/owner/repo.git/",
        ] {
            assert_eq!(normalize_remote_url(remote), "host/owner/repo");
        }
    }
    #[test]
    fn conflicting_roots_override_matching_remote() {
        let a = RepositoryIdentity {
            root_commits: vec!["a".repeat(40)],
            origin_url: Some("host/repo".into()),
        };
        let b = RepositoryIdentity {
            root_commits: vec!["b".repeat(40)],
            origin_url: a.origin_url.clone(),
        };
        assert_eq!(
            compare_repository_identities(Some(&a), Some(&b)),
            "different"
        );
        assert_eq!(compare_repository_identities(Some(&a), None), "unknown");
    }
}
