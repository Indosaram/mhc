use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::open_file_holders::{
    HolderLookupOptions, LSOF_ANSWER_BOUND_MS, answer_from_proc_or_lsof, current_uid,
    run_lsof_bounded,
};
use super::proc_view::{EntryVerdict, ProcScan, ProcView, classify_entry_error, scan_every_process};

struct RepoTree {
    roots: Vec<PathBuf>,
    dev: u64,
    ino: u64,
}

fn inside(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path == root || path.starts_with(root))
}

fn repo_tree(dir: &Path) -> Option<RepoTree> {
    let identity = crate::fs::metadata(dir).ok()?;
    #[cfg(unix)]
    let (dev, ino) = {
        use std::os::unix::fs::MetadataExt;
        (identity.dev(), identity.ino())
    };
    #[cfg(not(unix))]
    let (dev, ino) = (0_u64, 0_u64);

    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let resolved = crate::support::paths::resolve_from(&base, dir);
    let mut roots = vec![resolved];
    if let Ok(real) = std::fs::canonicalize(dir)
        && !roots.contains(&real)
    {
        roots.push(real);
    }
    Some(RepoTree { roots, dev, ino })
}

fn cwd_verdict(view: &ProcView, pid: u32, tree: &RepoTree) -> EntryVerdict {
    if pid == view.self_pid {
        return EntryVerdict::NoMatch;
    }
    let cwd_link = view.root.join(pid.to_string()).join("cwd");
    let target = match std::fs::read_link(&cwd_link) {
        Ok(target) => target,
        Err(error) => return classify_entry_error(view, pid, &error),
    };
    if inside(&target, &tree.roots) {
        return EntryVerdict::Match;
    }
    match crate::fs::metadata(&cwd_link) {
        Ok(cwd) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if cwd.dev() == tree.dev && cwd.ino() == tree.ino {
                    EntryVerdict::Match
                } else {
                    EntryVerdict::NoMatch
                }
            }
            #[cfg(not(unix))]
            {
                let _ = (cwd, tree.dev, tree.ino);
                EntryVerdict::NoMatch
            }
        }
        Err(error) => {
            if error.kind() == std::io::ErrorKind::NotFound {
                EntryVerdict::NoMatch
            } else {
                classify_entry_error(view, pid, &error)
            }
        }
    }
}

pub fn scan_proc_cwd_users(dir: &Path, view: &ProcView) -> ProcScan {
    let Some(tree) = repo_tree(dir) else {
        return ProcScan::Indeterminate;
    };
    scan_every_process(view, |pid| cwd_verdict(view, pid, &tree))
}

fn scan_lsof_cwd_users(dir: &Path, bound_ms: u64) -> Option<Vec<u32>> {
    let tree = repo_tree(dir)?;
    let uid = current_uid()?;
    let uid_str = uid.to_string();
    let run = run_lsof_bounded(&["-w", "-a", "-d", "cwd", "-u", &uid_str, "-Fpn"], bound_ms)?;
    if run.exit_code != Some(0) {
        return None;
    }
    let self_pid = std::process::id();
    let mut pid: Option<u32> = None;
    let mut saw_self = false;
    let mut users = BTreeSet::new();
    for line in run.stdout.split('\n') {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix('p') {
            if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let parsed = rest.parse::<u32>().ok()?;
            pid = Some(parsed);
            if parsed == self_pid {
                saw_self = true;
            }
        } else if let Some(path_str) = line.strip_prefix('n')
            && let Some(current_pid) = pid
            && current_pid != self_pid
            && inside(Path::new(path_str), &tree.roots)
        {
            users.insert(current_pid);
        }
    }
    if saw_self {
        Some(users.into_iter().collect())
    } else {
        None
    }
}

pub fn find_processes_working_in(dir: &Path, options: HolderLookupOptions) -> Option<Vec<u32>> {
    let bound_ms = options.lsof_bound_ms.unwrap_or(LSOF_ANSWER_BOUND_MS);
    answer_from_proc_or_lsof(
        |view| scan_proc_cwd_users(dir, view),
        || scan_lsof_cwd_users(dir, bound_ms),
    )
}
