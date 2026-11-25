use super::cell_authority_registry::{
    CellRule, InvocationRegistration, InvocationSessionSnapshot, invocations, register_invocation,
};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct ResolvedInvocationSession {
    pub session_id: String,
    pub cwd: String,
    pub epoch: u32,
    pub active: bool,
    pub rules: Vec<CellRule>,
}

pub type InvocationSessionResolver<'a> = &'a dyn Fn(&str) -> Option<InvocationSessionSnapshot>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct CrossSessionAuthorityError {
    pub code: &'static str,
    pub enforcing_session_id: String,
    pub rule: Option<Box<CellRule>>,
    pub message: String,
}

pub fn register_invocation_session(
    session_id: &str,
    read: Arc<dyn Fn() -> InvocationSessionSnapshot + Send + Sync>,
) -> InvocationRegistration {
    register_invocation(session_id, read)
}

fn cross_session_refusal(
    enforcing_session_id: &str,
    rule: Option<CellRule>,
) -> CrossSessionAuthorityError {
    let message = match &rule {
        Some(rule) => format!(
            "Cross-session kernel tool refused while session {enforcing_session_id} enforces deny rules, including {} {} ({})",
            rule.permission, rule.pattern, rule.layer
        ),
        None => "Cross-session kernel tool refused: permission authority unavailable".into(),
    };
    CrossSessionAuthorityError {
        code: "cell_permission_denied",
        enforcing_session_id: enforcing_session_id.to_owned(),
        rule: rule.map(Box::new),
        message,
    }
}

pub fn invocation_session(
    owner_id: &str,
    owner: &InvocationSessionSnapshot,
    invoker_id: Option<&str>,
    resolve_session: Option<InvocationSessionResolver<'_>>,
) -> Result<ResolvedInvocationSession, CrossSessionAuthorityError> {
    let Some(invoker_id) = invoker_id.filter(|id| *id != owner_id) else {
        return Ok(ResolvedInvocationSession {
            session_id: owner_id.to_owned(),
            cwd: owner.cwd.clone(),
            epoch: owner.epoch,
            active: owner.active,
            rules: owner.rules.clone(),
        });
    };
    let registered = invocations(invoker_id);
    let invoker = resolve_session
        .and_then(|resolve| resolve(invoker_id))
        .or_else(|| registered.iter().find(|session| session.active).cloned())
        .or_else(|| registered.into_iter().next());
    if owner.active {
        return Err(cross_session_refusal(
            owner_id,
            owner.rules.first().cloned(),
        ));
    }
    if let Some(invoker_state) = &invoker
        && invoker_state.active
    {
        return Err(cross_session_refusal(
            invoker_id,
            invoker_state.rules.first().cloned(),
        ));
    }
    Ok(match invoker {
        Some(invoker) => ResolvedInvocationSession {
            session_id: invoker_id.to_owned(),
            cwd: invoker.cwd,
            epoch: invoker.epoch,
            active: invoker.active,
            rules: invoker.rules,
        },
        None => ResolvedInvocationSession {
            session_id: invoker_id.to_owned(),
            cwd: owner.cwd.clone(),
            epoch: 0,
            active: false,
            rules: Vec::new(),
        },
    })
}
