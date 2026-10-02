//! Session-scoped delivery guard from task/usage-guidance.ts.
use std::collections::HashSet;

pub const TASK_USAGE_GUIDANCE: &str = concat!(
    "<omo-senpi-task>\n",
    "Background task results are automatically delivered: an idle session is always woken, and a running turn receives them at its next tool boundary.\n",
    "- /tasks shows this session's children; task_output is for one midpoint status or transcript peek (mode:\"tail\" for recent output).\n",
    "- task_send always steers a message into the addressed child, while task_cancel ends it.\n",
    "- Team mail is steered into the recipient's running turn. Use task_send for updates; team mail never queues as editable follow-up work.\n",
    "If no independent work remains, end your turn.\n",
    "</omo-senpi-task>"
);

#[derive(Default)]
pub struct OncePerSessionGuard {
    seen: HashSet<String>,
}

impl OncePerSessionGuard {
    pub fn first_delivery(&mut self, session_id: &str) -> bool {
        self.seen.insert(session_id.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::OncePerSessionGuard;

    #[test]
    fn session_start_refire_never_repeats_delivery() {
        let mut guard = OncePerSessionGuard::default();
        assert!(guard.first_delivery("session-a"));
        assert!(!guard.first_delivery("session-a"));
        assert!(guard.first_delivery("session-b"));
        assert!(!guard.first_delivery("session-a"));
    }
}
