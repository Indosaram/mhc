use maho_core::session_activity::{SessionActivitySnapshot,is_session_busy_snapshot};
pub fn is_handoff_busy(snapshot: SessionActivitySnapshot) -> bool {
    is_session_busy_snapshot(SessionActivitySnapshot { has_active_wake_source: false, ..snapshot })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn durable_wake_source_does_not_hold_handoff() { assert!(!is_handoff_busy(SessionActivitySnapshot { has_active_wake_source:true,..Default::default() })); }
    #[test] fn owned_work_keeps_handoff_busy() { for snapshot in [SessionActivitySnapshot { is_streaming:true,..Default::default() },SessionActivitySnapshot { is_bash_running:true,..Default::default() },SessionActivitySnapshot { is_compacting:true,..Default::default() },SessionActivitySnapshot { has_session_work:true,..Default::default() }] { assert!(is_handoff_busy(snapshot)); } }
}
