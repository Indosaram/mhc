use crate::{commands::PanelExec, constants::GIT_TIMEOUT_MS, types::PanelGitStatus};
use super::parse::{merge_git_deltas, parse_numstat_z, parse_porcelain_z};

pub async fn read_git_status(exec: &dyn PanelExec, root: &str) -> Option<PanelGitStatus> {
    let args = |parts: &[&str]| parts.iter().map(|part| (*part).to_owned()).collect();
    let (status, unstaged, staged) = futures::join!(
        exec.exec("git", args(&["status", "--porcelain=v1", "-z", "--untracked-files=normal"]), root, GIT_TIMEOUT_MS),
        exec.exec("git", args(&["diff", "--numstat", "-z"]), root, GIT_TIMEOUT_MS),
        exec.exec("git", args(&["diff", "--cached", "--numstat", "-z"]), root, GIT_TIMEOUT_MS),
    );
    let status = status.ok().filter(|status| status.code == 0)?;
    Some(PanelGitStatus {
        root: root.into(),
        files: merge_git_deltas(
            &parse_porcelain_z(&status.stdout),
            &parse_numstat_z(unstaged.as_ref().map_or("", |result| result.stdout.as_str())),
            &parse_numstat_z(staged.as_ref().map_or("", |result| result.stdout.as_str())),
        ),
    })
}
