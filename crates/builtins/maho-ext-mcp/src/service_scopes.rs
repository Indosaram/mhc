//! Port of senpi mcp/service-scopes.ts (brain/hand S16c): one execution scope a binding serves beside
//! its own context. A runtime serving several scopes keeps each one's MCP forms, presentation, and
//! control inventory instead of the latest scope taking them.
use crate::request_origin::McpElicitationUiLookup;
use maho_core::mcp_mrtr_presentations::McpMrtrPresentation;
use maho_ext_api::ExtensionUi;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub struct McpBindingScope {
    pub session_id: String,
    pub presentation: Option<McpMrtrPresentation>,
    pub ui: McpElicitationUiLookup,
}

pub struct CallerScope {
    pub session_id: String,
    pub presentation: Option<McpMrtrPresentation>,
    pub ui: McpElicitationUiLookup,
}

/// A context's elicitation UI, read per request: a host that offers none answers no form (`has_ui` false).
pub fn elicitation_ui_of(ui: Option<Arc<dyn ExtensionUi>>, has_ui: bool) -> McpElicitationUiLookup {
    if !has_ui {
        return Arc::new(|| None);
    }
    Arc::new(move || ui.clone())
}

/// A further scope joins the binding and returns true; the binding's own session joins nothing.
pub fn join_binding_scope(
    binding_session_id: Option<&str>,
    scopes: &Mutex<BTreeMap<String, McpBindingScope>>,
    session_id: &str,
    ui: Option<Arc<dyn ExtensionUi>>,
    has_ui: bool,
) -> bool {
    if binding_session_id == Some(session_id) {
        return false;
    }
    let presentation = ui
        .as_ref()
        .and_then(maho_core::mcp_mrtr_presentations::get_mcp_mrtr_presentation);
    let scope = McpBindingScope {
        session_id: session_id.to_owned(),
        presentation,
        ui: elicitation_ui_of(ui, has_ui),
    };
    scopes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(session_id.to_owned(), scope);
    true
}

/// A joined scope presents its own forms; the binding's own context keeps the binding's presentation.
pub fn bind_scope_presentation(
    binding_presentation: &Mutex<Option<McpMrtrPresentation>>,
    scopes: &Mutex<BTreeMap<String, McpBindingScope>>,
    presentation: McpMrtrPresentation,
    session_id: Option<&str>,
) {
    if let Some(session_id) = session_id {
        let mut scopes = scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(scope) = scopes.get_mut(session_id) {
            scope.presentation = Some(presentation);
            return;
        }
    }
    *binding_presentation
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(presentation);
}

pub fn binding_serves_scope(
    scopes: &Mutex<BTreeMap<String, McpBindingScope>>,
    session_id: Option<&str>,
) -> bool {
    let Some(session_id) = session_id else {
        return false;
    };
    scopes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains_key(session_id)
}

/// The scope a call belongs to while its binding serves several. A binding serving its own context alone
/// resolves to `None`, and its requests carry no scope: the single-session path is unchanged.
pub fn caller_scope(
    binding_session_id: Option<&str>,
    binding_presentation: &Mutex<Option<McpMrtrPresentation>>,
    binding_ui: &McpElicitationUiLookup,
    scopes: &Mutex<BTreeMap<String, McpBindingScope>>,
    caller_session_id: Option<&str>,
) -> Option<CallerScope> {
    let caller_id = caller_session_id?;
    let scopes = scopes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if scopes.is_empty() {
        return None;
    }
    if let Some(joined) = scopes.get(caller_id) {
        return Some(CallerScope {
            session_id: joined.session_id.clone(),
            presentation: joined.presentation.clone(),
            ui: joined.ui.clone(),
        });
    }
    if binding_session_id == Some(caller_id) {
        return Some(CallerScope {
            session_id: caller_id.to_owned(),
            presentation: binding_presentation
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone(),
            ui: binding_ui.clone(),
        });
    }
    None
}
