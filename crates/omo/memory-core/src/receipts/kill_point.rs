//! Crash injection for recovery tests: `OMO_MEMORY_KILL_POINT=<point>` makes the process
//! that reaches that point kill itself without cleanup. The variable is read at call time,
//! so the only isolation is the process boundary: set it in a child's own environment, never
//! in a process that runs maintenance code.

pub const MEMORY_KILL_POINTS: [&str; 7] = [
    "after-reserve",
    "after-prelaunch",
    "after-worktree",
    "after-child-exit",
    "after-validate",
    "after-merge",
    "before-receipt",
];

pub type MemoryKillPoint = &'static str;

/// Injected kill function: `(pid, signal)`.
pub type KillFn<'a> = &'a dyn Fn(u32, Option<&str>);

#[derive(Default)]
pub struct KillPointOptions<'a> {
    pub kill: Option<KillFn<'a>>,
    pub platform: Option<&'a str>,
    pub env_var: Option<&'a str>,
}

/// Kills this process when `OMO_MEMORY_KILL_POINT` names `point`. Returns how it killed (a POSIX
/// `"signal"`, or Windows `"terminate"`), or `None` when it did not match.
pub fn maybe_kill_at(point: &str, options: KillPointOptions<'_>) -> Option<&'static str> {
    let active = match options.env_var {
        Some(val) => val.to_string(),
        None => std::env::var("OMO_MEMORY_KILL_POINT").ok()?,
    };
    if active != point {
        return None;
    }

    let pid = std::process::id();
    let platform = options.platform.unwrap_or(if cfg!(windows) {
        "win32"
    } else {
        "linux"
    });

    if let Some(kill) = options.kill {
        if platform == "win32" {
            kill(pid, None);
            return Some("terminate");
        }
        kill(pid, Some("SIGKILL"));
        return Some("signal");
    }

    // Process kill via std::process::Command to keep safe code
    #[cfg(unix)]
    {
        if platform == "win32" {
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .status();
            Some("terminate")
        } else {
            let _ = std::process::Command::new("kill")
                .args(["-s", "KILL", &pid.to_string()])
                .status();
            Some("signal")
        }
    }

    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .status();
        Some("terminate")
    }
}
