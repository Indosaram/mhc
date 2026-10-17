use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcScan {
    Holders(Vec<u32>),
    Indeterminate,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcView {
    pub root: PathBuf,
    pub self_pid: u32,
    pub self_uid: u32,
}

pub fn live_proc_view(self_uid: u32) -> ProcView {
    ProcView {
        root: PathBuf::from("/proc"),
        self_pid: std::process::id(),
        self_uid,
    }
}

pub fn unreadable_fd_dir_means_held(owner_uid: u32, self_uid: u32) -> bool {
    owner_uid == self_uid
}

pub fn list_proc_pids(view: &ProcView) -> Option<Vec<u32>> {
    let entries = crate::fs::read_dir_names(&view.root).ok()?;
    let self_pid_str = view.self_pid.to_string();
    if !entries.iter().any(|entry| entry == &self_pid_str) {
        return None;
    }
    let mut pids = Vec::new();
    for entry in entries {
        if !entry.is_empty()
            && entry.bytes().all(|b| b.is_ascii_digit())
            && let Ok(pid) = entry.parse::<u32>()
        {
            pids.push(pid);
        }
    }
    Some(pids)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryVerdict {
    Match,
    NoMatch,
    Gone,
    Indeterminate,
}

pub fn classify_entry_error(view: &ProcView, pid: u32, error: &std::io::Error) -> EntryVerdict {
    if error.kind() == std::io::ErrorKind::NotFound {
        return EntryVerdict::Gone;
    }
    #[cfg(unix)]
    if matches!(error.raw_os_error(), Some(20 | 3)) {
        return EntryVerdict::Gone;
    }
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        let Ok(owner) = crate::fs::metadata(&view.root.join(pid.to_string())) else {
            return EntryVerdict::Gone;
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            return if unreadable_fd_dir_means_held(owner.uid(), view.self_uid) {
                EntryVerdict::Indeterminate
            } else {
                EntryVerdict::Gone
            };
        }
        #[cfg(not(unix))]
        {
            let _ = owner;
            return EntryVerdict::Indeterminate;
        }
    }
    EntryVerdict::Indeterminate
}

pub fn scan_every_process(
    view: &ProcView,
    inspect: impl Fn(u32) -> EntryVerdict,
) -> ProcScan {
    let Some(pids) = list_proc_pids(view) else {
        return ProcScan::Unavailable;
    };
    let mut holders = Vec::new();
    for pid in pids {
        match inspect(pid) {
            EntryVerdict::Indeterminate => return ProcScan::Indeterminate,
            EntryVerdict::Match => holders.push(pid),
            EntryVerdict::NoMatch | EntryVerdict::Gone => {}
        }
    }
    holders.sort_unstable();
    holders.dedup();
    ProcScan::Holders(holders)
}
