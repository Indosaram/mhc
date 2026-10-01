//! Port of `tools/control/caller-session.ts`.

use crate::tools::control::types::SessionIdCarrier;

/// The component wires this from the live extension context. The control tools ALWAYS call it and
/// pass the result into every steering/list call so the scope guard is never fail-open in production.
pub fn default_resolve_caller_session_id(ctx: &dyn SessionIdCarrier) -> Option<String> {
    Some(ctx.session_manager().get_session_id())
}
