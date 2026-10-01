//! Port of senpi packages/ai/src/tool-call-middleware/recovery-content-lifecycle.ts.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryContentKind {
    Text,
    Thinking,
    ToolCall,
}

#[derive(Clone, Copy)]
struct ActiveContent {
    inner_index: usize,
    kind: RecoveryContentKind,
}

#[derive(Default)]
pub struct RecoveryContentLifecycle {
    active: Option<ActiveContent>,
    last_ended_inner_index: i64,
}

impl RecoveryContentLifecycle {
    pub fn new() -> Self {
        Self { active: None, last_ended_inner_index: -1 }
    }

    pub fn can_start(&self, inner_index: usize) -> bool {
        self.active.is_none() && inner_index as i64 > self.last_ended_inner_index
    }

    pub fn start(&mut self, inner_index: usize, kind: RecoveryContentKind) {
        self.active = Some(ActiveContent { inner_index, kind });
    }

    pub fn is_active(&self, inner_index: usize, kind: RecoveryContentKind) -> bool {
        self.active.is_some_and(|active| active.inner_index == inner_index && active.kind == kind)
    }

    pub fn end(&mut self, inner_index: usize, kind: RecoveryContentKind) -> bool {
        if !self.is_active(inner_index, kind) {
            return false;
        }
        self.active = None;
        self.last_ended_inner_index = inner_index as i64;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_start_before_anything_has_run() {
        let lifecycle = RecoveryContentLifecycle::new();
        assert!(lifecycle.can_start(0));
    }

    #[test]
    fn cannot_start_while_another_content_kind_is_active() {
        let mut lifecycle = RecoveryContentLifecycle::new();
        lifecycle.start(0, RecoveryContentKind::Text);
        assert!(!lifecycle.can_start(1));
    }

    #[test]
    fn cannot_restart_an_already_ended_index() {
        let mut lifecycle = RecoveryContentLifecycle::new();
        lifecycle.start(0, RecoveryContentKind::Text);
        assert!(lifecycle.end(0, RecoveryContentKind::Text));
        assert!(!lifecycle.can_start(0));
        assert!(lifecycle.can_start(1));
    }

    #[test]
    fn end_fails_for_a_mismatched_kind_or_index() {
        let mut lifecycle = RecoveryContentLifecycle::new();
        lifecycle.start(0, RecoveryContentKind::Text);
        assert!(!lifecycle.end(0, RecoveryContentKind::ToolCall));
        assert!(!lifecycle.end(1, RecoveryContentKind::Text));
        assert!(lifecycle.end(0, RecoveryContentKind::Text));
    }
}
