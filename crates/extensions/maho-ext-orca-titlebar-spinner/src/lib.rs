//! Native port of the Orca titlebar spinner.
use maho_ext_api::{EventKind, EventResult, Extension, ExtensionApi, ExtensionContext};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

const FRAMES: [&str; 10] = ["\u{280b}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283c}", "\u{2834}", "\u{2826}", "\u{2827}", "\u{2807}", "\u{280f}"];

fn base_title(cwd: &str, session: Option<&str>) -> String {
    let directory = cwd.split(['/', '\\']).rfind(|part| !part.is_empty()).unwrap_or(cwd);
    match session.filter(|name| !name.is_empty()) {
        Some(name) => format!("\u{03c0} - {name} - {directory}"),
        None => format!("\u{03c0} - {directory}"),
    }
}

struct Animation { stop: mpsc::Sender<()>, thread: Option<JoinHandle<()>> }
impl Drop for Animation {
    fn drop(&mut self) {
        if let Err(error) = self.stop.send(()) { eprintln!("orca spinner stop: {error}"); }
        if let Some(thread) = self.thread.take()
            && thread.join().is_err() {
            eprintln!("orca spinner thread panicked");
        }
    }
}

fn stop_animation(state: &Mutex<Option<Animation>>, ctx: &ExtensionContext) {
    if let Some(animation) = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() {
        drop(animation);
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| ctx.cwd.clone());
    ctx.ui.set_title(&base_title(&cwd.to_string_lossy(), ctx.session_manager.get_session_name().as_deref()));
}

/// Statically registered Orca titlebar extension.
pub struct OrcaTitlebarSpinner;
impl Extension for OrcaTitlebarSpinner {
    fn register(&self, api: &mut ExtensionApi) {
        if std::env::var("ORCA_PANE_KEY").unwrap_or_default().is_empty() { return; }
        let state = Arc::new(Mutex::new(None));
        let start_state = Arc::clone(&state);
        api.on(EventKind::AgentStart, Arc::new(move |_, ctx| {
            let state = Arc::clone(&start_state);
            Box::pin(async move {
                stop_animation(&state, ctx);
                let (stop, receiver) = mpsc::channel();
                let ui = Arc::clone(&ctx.ui);
                let session = Arc::clone(&ctx.session_manager);
                let cwd = ctx.cwd.clone();
                let thread = std::thread::spawn(move || {
                    let mut index = 0;
                    while matches!(receiver.recv_timeout(Duration::from_millis(80)), Err(mpsc::RecvTimeoutError::Timeout)) {
                        let cwd = std::env::current_dir().unwrap_or_else(|_| cwd.clone());
                        ui.set_title(&format!("{} {}", FRAMES[index], base_title(&cwd.to_string_lossy(), session.get_session_name().as_deref())));
                        index = (index + 1) % FRAMES.len();
                    }
                });
                *state.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Animation { stop, thread: Some(thread) });
                Ok(EventResult::None)
            })
        }));
        for event in [EventKind::AgentEnd, EventKind::SessionShutdown] {
            let state = Arc::clone(&state);
            api.on(event, Arc::new(move |_, ctx| {
                let state = Arc::clone(&state);
                Box::pin(async move { stop_animation(&state, ctx); Ok(EventResult::None) })
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn title_when_session_named() { assert_eq!(base_title("/work/project/", Some("chat")), "\u{03c0} - chat - project"); }
    #[test]
    fn title_when_windows_path() { assert_eq!(base_title("C:\\work\\project", None), "\u{03c0} - project"); }
    #[test]
    fn title_when_root_path() { assert_eq!(base_title("/", Some("")), "\u{03c0} - /"); }
}
