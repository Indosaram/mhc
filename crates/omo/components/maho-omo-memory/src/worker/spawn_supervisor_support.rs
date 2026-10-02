use std::{collections::BTreeMap, path::Path};

pub fn defined_environment(env: &BTreeMap<String, Option<String>>) -> BTreeMap<String, String> {
    env.iter().filter_map(|(key, value)| value.as_ref().map(|value| (key.clone(), value.clone()))).collect()
}

pub fn is_node_signal(signal: Option<&str>) -> bool {
    signal.is_some_and(|signal| ["SIGABRT", "SIGALRM", "SIGBUS", "SIGCHLD", "SIGCONT", "SIGFPE", "SIGHUP", "SIGILL", "SIGINT", "SIGIO", "SIGIOT", "SIGKILL", "SIGPIPE", "SIGPOLL", "SIGPROF", "SIGPWR", "SIGQUIT", "SIGSEGV", "SIGSTKFLT", "SIGSTOP", "SIGSYS", "SIGTERM", "SIGTRAP", "SIGTSTP", "SIGTTIN", "SIGTTOU", "SIGURG", "SIGUSR1", "SIGUSR2", "SIGVTALRM", "SIGWINCH", "SIGXCPU", "SIGXFSZ"].contains(&signal))
}

pub fn read_tail(path: &Path, max_bytes: usize) -> String {
    match std::fs::read_to_string(path) {
        Ok(content) if content.len() <= max_bytes => content,
        Ok(content) => {
            let utf16: Vec<_> = content.encode_utf16().collect();
            format!("[truncated to last {max_bytes} bytes]\n{}", String::from_utf16_lossy(&utf16[utf16.len().saturating_sub(max_bytes)..]))
        }
        Err(error) => format!("[failed to read child output: {error}]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn undefined_environment_is_removed_but_empty_values_survive() { assert_eq!(defined_environment(&BTreeMap::from([("empty".into(),Some(String::new())),("missing".into(),None)])), BTreeMap::from([("empty".into(),String::new())])); }
    #[test] fn only_known_node_signals_survive() { assert!(is_node_signal(Some("SIGTERM"))); assert!(is_node_signal(Some("SIGIOT"))); assert!(!is_node_signal(Some("MODEL_PID"))); assert!(!is_node_signal(None)); }
    #[test] fn output_tail_matches_utf16_source_slice() { let root=tempfile::tempdir().unwrap(); let path=root.path().join("stdout"); std::fs::write(&path,"abcde").unwrap(); assert_eq!(read_tail(&path,3),"[truncated to last 3 bytes]\ncde"); std::fs::write(&path,"한글").unwrap(); assert_eq!(read_tail(&path,3),"[truncated to last 3 bytes]\n한글"); assert!(read_tail(&root.path().join("missing"),3).starts_with("[failed to read child output:")); }
}
