//! Port of senpi `cli/session-opening.ts` (pin 903b8336).
//!
//! An interactive launch warns, after the runtime publishes its own holder, when another process
//! still holds the session. The lookup is advisory and never aborts startup. senpi calls
//! `sessionHolderWarning(file, id)` directly. Here the caller passes the lookup, which is normally
//! `maho_core::foreign_session_holders::session_holder_warning` over the host's probes.

use std::future::Future;

use maho_core::project_trust::AppMode;

/// senpi `prepareSessionOpening`. `warn` receives the coloured `Warning: ...` line that senpi
/// prints with `console.error`.
pub async fn prepare_session_opening<F, Fut>(
    mode: AppMode,
    holder_warning: F,
    warn: &mut dyn FnMut(&str),
) where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Option<String>>,
{
    if mode != AppMode::Interactive {
        return;
    }
    if let Some(warning) = holder_warning().await {
        warn(&format!("\x1b[33mWarning: {warning}\x1b[39m"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(mode: AppMode, warning: Option<&str>) -> (Vec<String>, bool) {
        let mut lines = Vec::new();
        let mut asked = false;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime.block_on(prepare_session_opening(
            mode,
            || {
                asked = true;
                std::future::ready(warning.map(str::to_owned))
            },
            &mut |line: &str| lines.push(line.to_owned()),
        ));
        (lines, asked)
    }

    #[test]
    fn an_interactive_launch_prints_the_holder_warning() {
        let (lines, asked) = run(AppMode::Interactive, Some("pid 7 holds this session"));
        assert!(asked);
        assert_eq!(lines, ["\x1b[33mWarning: pid 7 holds this session\x1b[39m"]);
        assert!(run(AppMode::Interactive, None).0.is_empty());
    }

    #[test]
    fn other_modes_never_look_up_holders() {
        for mode in [AppMode::Print, AppMode::Json, AppMode::Rpc] {
            let (lines, asked) = run(mode, Some("ignored"));
            assert!(!asked);
            assert!(lines.is_empty());
        }
    }
}
