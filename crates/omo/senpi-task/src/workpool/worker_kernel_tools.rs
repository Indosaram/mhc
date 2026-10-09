use super::types::{WorkpoolError, WorkpoolErrorCode, WorkpoolRecord, WorkpoolSpec};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelToolGrant { pub names: Vec<String> }

pub trait KernelToolCapability { fn available(&self, name: &str) -> bool; }

fn denied_code(code: &str) -> WorkpoolErrorCode {
    match code { "policy_denied" => WorkpoolErrorCode::PolicyDenied, "reserved_tool_name" => WorkpoolErrorCode::ReservedToolName, "tool_name_collision" => WorkpoolErrorCode::ToolNameCollision, "kernel_tool_missing" => WorkpoolErrorCode::KernelToolMissing, "kernel_tool_stale" => WorkpoolErrorCode::KernelToolStale, _ => WorkpoolErrorCode::ToolsUnavailable }
}

pub fn resolve_pool_kernel_tools(
    names: &[String], capability: &dyn KernelToolCapability, _spec: &WorkpoolSpec, existing: &[String], allow: Option<&[String]>, deny: Option<&[String]>,
) -> Result<Option<KernelToolGrant>, WorkpoolError> {
    if names.is_empty() { return Ok(None); }
    let mut granted = Vec::with_capacity(names.len());
    for name in names {
        if existing.iter().any(|other| other == name) { return Err(WorkpoolError::new(denied_code("tool_name_collision"), format!("Tool name collision: {name}"))); }
        if deny.is_some_and(|items| items.iter().any(|item| item == name)) || allow.is_some_and(|items| !items.iter().any(|item| item == name)) { return Err(WorkpoolError::new(denied_code("policy_denied"), format!("Kernel tool denied: {name}"))); }
        if !capability.available(name) { return Err(WorkpoolError::new(denied_code("kernel_tool_missing"), format!("Kernel tool is unavailable: {name}"))); }
        granted.push(name.clone());
    }
    Ok(Some(KernelToolGrant { names: granted }))
}

pub fn resolve_worker_kernel_tools(pool: &WorkpoolRecord, capability: Option<&dyn KernelToolCapability>, existing: &[String]) -> Result<Option<KernelToolGrant>, WorkpoolError> {
    let names = pool.kernel_tool_names.as_deref().unwrap_or_default();
    if names.is_empty() { return Ok(None); }
    let capability = capability.ok_or_else(|| WorkpoolError::new(WorkpoolErrorCode::ToolsUnavailable, format!("Pool {} has no live parent kernel binding for its worker tools; recreate the pool from a live JavaScript cell.", pool.pool_id)))?;
    resolve_pool_kernel_tools(names, capability, &pool.worker_spec, existing, pool.worker_spec.plan.tool_allowlist.as_deref(), pool.worker_spec.plan.tool_denylist.as_deref())
}

#[cfg(test)] mod tests { use super::*; struct Fake; impl KernelToolCapability for Fake { fn available(&self, name: &str) -> bool { name == "lookup" } } #[test] fn missing_capability_is_typed() { let result = resolve_worker_kernel_tools(&WorkpoolRecord { version: 1, pool_id: "wp_test".into(), name: "n".into(), parent_session_id: "p".into(), root_session_id: "p".into(), generation: 0, revision: 0, mode: super::super::types::WorkpoolMode::Fresh, agent: super::super::types::WorkpoolAgent::Category { prompt: "p".into(), model: None, category: "c".into() }, worker_spec: WorkpoolSpec { start: Default::default(), plan: Default::default() }, status: super::super::types::WorkpoolStatus::Open, kernel_tool_names: Some(vec!["lookup".into()]), items: vec![], workers: vec![], aggregate: None }, None, &[]); assert_eq!(result.unwrap_err().code, WorkpoolErrorCode::ToolsUnavailable); } }
