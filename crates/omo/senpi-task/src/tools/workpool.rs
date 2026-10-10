use serde_json::Value;
use std::sync::Arc;

use crate::tools::workpool_schema::{WORKPOOL_PARAMS, WorkpoolToolParams};
use crate::workpool::types::{WorkpoolCaller, WorkpoolError};

pub const WORKPOOL_TOOL_NAME: &str = "workpool";
pub const WORKPOOL_TOOL_LABEL: &str = "Workpool";
pub const WORKPOOL_DESCRIPTION: &str = "Create and inspect engine-owned keyed work queues. Push returns durable IDs without waiting for capacity. Omitted mode uses the approved keep_alive default; fresh remains selectable. Inspect reads durable keyed data or errors; close delivers one aggregate; uncertain delivery is never retried automatically.";

pub trait WorkpoolToolBackend: Send + Sync {
    fn execute(&self, caller: &WorkpoolCaller, input: WorkpoolToolParams) -> Result<Value, WorkpoolError>;
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

    pub fn execute_json(&self, caller: &WorkpoolCaller, value: Value) -> Result<Value, WorkpoolError> {
        let input = crate::workpool::schema::parse_input(value)?;
        self.backend.execute(caller, input)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::workpool_schema::WorkpoolOperation;

    struct Backend;
    impl WorkpoolToolBackend for Backend {
        fn execute(&self, _caller: &WorkpoolCaller, input: WorkpoolToolParams) -> Result<Value, WorkpoolError> {
            Ok(serde_json::json!({"op": input.op}))
        }
    }

    #[test]
    fn tool_uses_workpool_name_and_label() {
        let tool = WorkpoolTool::new(Arc::new(Backend));
        assert_eq!(tool.name, "workpool");
        assert_eq!(tool.label, "Workpool");
    }

    #[test]
    fn tool_decodes_operation_before_backend_execution() {
        let tool = WorkpoolTool::new(Arc::new(Backend));
        let caller = WorkpoolCaller { session_id: "s".into(), root_session_id: "s".into(), depth: 0, cwd: "/".into() };
        let result = tool.execute_json(&caller, serde_json::json!({"op":"inspect"})).expect("valid tool input");
        assert_eq!(result["op"], serde_json::json!(WorkpoolOperation::Inspect));
    }
}

