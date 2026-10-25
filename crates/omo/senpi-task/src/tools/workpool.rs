use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::tools::workpool_schema::{WORKPOOL_PARAMS, WorkpoolToolParams};
use crate::workpool::types::{WorkpoolCaller, WorkpoolError};

pub const WORKPOOL_TOOL_NAME: &str = "workpool";
pub const WORKPOOL_TOOL_LABEL: &str = "Workpool";
pub const WORKPOOL_DESCRIPTION: &str = "Create and inspect engine-owned keyed work queues. Push returns durable IDs without waiting for capacity. Omitted mode uses the approved keep_alive default; fresh remains selectable. Inspect reads durable keyed data or errors; close delivers one aggregate; uncertain delivery is never retried automatically.";

pub trait WorkpoolToolBackend: Send + Sync {
    fn attach<'a>(&'a self, _session_id: &'a str) -> Pin<Box<dyn Future<Output = Result<(), WorkpoolError>> + Send + 'a>> { Box::pin(async { Ok(()) }) }
    fn execute<'a>(&'a self, caller: &'a WorkpoolCaller, input: WorkpoolToolParams) -> Pin<Box<dyn Future<Output = Result<Value, WorkpoolError>> + Send + 'a>>;
}

pub struct WorkpoolTool {
    pub name: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub parameters: Value,
    backend: Arc<dyn WorkpoolToolBackend>,
}
impl WorkpoolTool {
    pub fn new(backend: Arc<dyn WorkpoolToolBackend>) -> Self {
        Self { name: WORKPOOL_TOOL_NAME, label: WORKPOOL_TOOL_LABEL, description: WORKPOOL_DESCRIPTION, parameters: WORKPOOL_PARAMS.clone(), backend }
    }

    pub async fn execute_json(&self, caller: &WorkpoolCaller, value: Value) -> Result<Value, WorkpoolError> {
        let input = crate::workpool::schema::parse_command(value)?;
        self.backend.execute(caller, input).await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::workpool_schema::WorkpoolOperation;

    struct Backend;
    impl WorkpoolToolBackend for Backend {
        fn execute<'a>(&'a self, _caller: &'a WorkpoolCaller, input: WorkpoolToolParams) -> Pin<Box<dyn Future<Output = Result<Value, WorkpoolError>> + Send + 'a>> {
            Box::pin(async move { Ok(serde_json::json!({"op": input.op})) })
        }
    }

    #[test]
    fn tool_uses_workpool_name_and_label() {
        let tool = WorkpoolTool::new(Arc::new(Backend));
        assert_eq!(tool.name, "workpool");
        assert_eq!(tool.label, "Workpool");
    }

    #[tokio::test]
    async fn tool_decodes_operation_before_backend_execution() {
        let tool = WorkpoolTool::new(Arc::new(Backend));
        let caller = WorkpoolCaller { session_id: "s".into(), root_session_id: "s".into(), depth: 0, cwd: "/".into() };
        let result = tool.execute_json(&caller, serde_json::json!({"op":"inspect","pool_id":"wp_00000000000000000000000000000000"})).await.expect("valid tool input");
        assert_eq!(result["op"], serde_json::json!(WorkpoolOperation::Inspect));
    }

    #[tokio::test]
    async fn strict_commands_reject_missing_fields_and_cross_operation_fields() {
        let tool = WorkpoolTool::new(Arc::new(Backend));
        let caller = WorkpoolCaller { session_id: "s".into(), root_session_id: "s".into(), depth: 0, cwd: "/".into() };
        for input in [
            serde_json::json!({"op":"inspect"}),
            serde_json::json!({"op":"inspect","pool_id":"wp_00000000000000000000000000000000","name":"unexpected"}),
            serde_json::json!({"op":"create","name":"pool","agent":{"category":"quick","subagent_type":"worker","prompt":"run"}}),
            serde_json::json!({"op":"push","pool_id":"wp_00000000000000000000000000000000","items":[{"key":" ","input":null}]}),
            serde_json::json!({"op":"yield","results":[{"key":"a","data":null,"error":{"code":"x","message":"x"}}]}),
        ] {
            assert_eq!(tool.execute_json(&caller, input).await.expect_err("invalid command").code, crate::workpool::types::WorkpoolErrorCode::InvalidInput);
        }
        let parsed = crate::workpool::schema::parse_command(serde_json::json!({"op":"create","name":" pool ","agent":{"category":" quick ","prompt":" run "}})).expect("trimmed command");
        assert_eq!(parsed.name.as_deref(), Some("pool"));
        assert!(matches!(parsed.agent, Some(crate::tools::workpool_schema::WorkpoolToolAgent::Category { category, prompt, .. }) if category == "quick" && prompt == "run"));
    }
}

