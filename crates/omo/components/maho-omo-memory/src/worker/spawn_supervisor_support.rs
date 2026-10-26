use std::{collections::BTreeMap, path::Path};

pub fn run_memory_hook(args: &[String], cwd: &Path) -> i32 {
    memory_core::memfs::memory_hook::run_memory_hook(args, cwd)
}

pub fn install_memory_hooks(
    repo: &Path,
) -> std::io::Result<Vec<memory_core::memfs::hooks::InstalledHook>> {
    let mut executable = std::env::current_exe()?;
    if executable.file_stem().and_then(|name| name.to_str()) != Some("mhc")
        && let Some(parent) = executable.parent() {
            executable = parent.parent().unwrap_or(parent).join(if cfg!(windows) {
                "mhc.exe"
            } else {
                "mhc"
            });
        }
    let path = executable.to_string_lossy().into_owned();
    memory_core::memfs::hooks::install_hooks_with_options(
        repo,
        &memory_core::memfs::hooks::InstallHooksOptions {
            checker: Some(memory_core::memfs::hooks::MemoryHookChecker {
                exec_path: path,
                script: String::new(),
            }),
        },
    )
}

use memory_core::memfs::MemoryHookChecker;

/// The flag the engine-owned native entry answers the memory hooks with (`memory-run-supervisor.ts`
/// `--memory-hook`); the hook script runs `<executable> <this flag> --memory-hook <stage> [file]`.
pub const MEMORY_HOOK_ENTRY_FLAG: &str = "--memory-supervisor";

/// The memory hooks run the staged-commit checker through the engine's own runtime and the bundled
/// supervisor entry, the same way the supervisor itself is spawned (`memoryHookChecker`).
pub fn memory_hook_checker() -> MemoryHookChecker {
    MemoryHookChecker {
        exec_path: std::env::current_exe()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default(),
        script: MEMORY_HOOK_ENTRY_FLAG.to_string(),
    }
}

pub fn defined_environment(env: &BTreeMap<String, Option<String>>) -> BTreeMap<String, String> {
    env.iter()
        .filter_map(|(key, value)| value.as_ref().map(|value| (key.clone(), value.clone())))
        .collect()
}

pub fn is_node_signal(signal: Option<&str>) -> bool {
    signal.is_some_and(|signal| {
        [
            "SIGABRT",
            "SIGALRM",
            "SIGBUS",
            "SIGCHLD",
            "SIGCONT",
            "SIGFPE",
            "SIGHUP",
            "SIGILL",
            "SIGINT",
            "SIGIO",
            "SIGIOT",
            "SIGKILL",
            "SIGPIPE",
            "SIGPOLL",
            "SIGPROF",
            "SIGPWR",
            "SIGQUIT",
            "SIGSEGV",
            "SIGSTKFLT",
            "SIGSTOP",
            "SIGSYS",
            "SIGTERM",
            "SIGTRAP",
            "SIGTSTP",
            "SIGTTIN",
            "SIGTTOU",
            "SIGURG",
            "SIGUSR1",
            "SIGUSR2",
            "SIGVTALRM",
            "SIGWINCH",
            "SIGXCPU",
            "SIGXFSZ",
        ]
        .contains(&signal)
    })
}

pub fn read_tail(path: &Path, max_bytes: usize) -> String {
    match std::fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned()) {
        Ok(content) if content.len() <= max_bytes => content,
        Ok(content) => {
            let utf16: Vec<_> = content.encode_utf16().collect();
            let start = if max_bytes == 0 {
                0
            } else {
                utf16.len().saturating_sub(max_bytes)
            };
            format!(
                "[truncated to last {max_bytes} bytes]\n{}",
                String::from_utf16_lossy(&utf16[start..])
            )
        }
        Err(error) => format!("[failed to read child output: {error}]"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_utf8_output_is_decoded_with_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("stdout");
        std::fs::write(&path, [b'a', 0xff, b'b']).unwrap();
        assert_eq!(read_tail(&path, 10), "a\u{fffd}b");
    }
    #[test]
    fn zero_tail_limit_preserves_source_negative_zero_slice() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("stdout");
        std::fs::write(&path, "abc").unwrap();
        assert_eq!(read_tail(&path, 0), "[truncated to last 0 bytes]\nabc");
    }
    #[test]
    fn undefined_environment_is_removed_but_empty_values_survive() {
        assert_eq!(
            defined_environment(&BTreeMap::from([
                ("empty".into(), Some(String::new())),
                ("missing".into(), None)
            ])),
            BTreeMap::from([("empty".into(), String::new())])
        );
    }
    #[test]
    fn the_memory_hook_checker_points_at_the_engine_entry() {
        let checker = memory_hook_checker();
        assert_eq!(checker.script, MEMORY_HOOK_ENTRY_FLAG);
        assert!(!checker.exec_path.is_empty());
    }
    #[test]
    fn only_known_node_signals_survive() {
        assert!(is_node_signal(Some("SIGTERM")));
        assert!(is_node_signal(Some("SIGIOT")));
        assert!(!is_node_signal(Some("MODEL_PID")));
        assert!(!is_node_signal(None));
    }
    #[test]
    fn output_tail_matches_utf16_source_slice() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("stdout");
        std::fs::write(&path, "abcde").unwrap();
        assert_eq!(read_tail(&path, 3), "[truncated to last 3 bytes]\ncde");
        std::fs::write(&path, "한글").unwrap();
        assert_eq!(read_tail(&path, 3), "[truncated to last 3 bytes]\n한글");
        assert!(
            read_tail(&root.path().join("missing"), 3).starts_with("[failed to read child output:")
        );
    }
}
