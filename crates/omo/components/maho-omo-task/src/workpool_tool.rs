use std::sync::Arc;

use maho_ext_api::{ExtensionApi, ToolContent, ToolDefinition, ToolError, ToolResult};
use senpi_task::{
    tools::workpool::{WorkpoolTool, WorkpoolToolBackend},
    workpool::types::WorkpoolCaller,
};

pub fn register_workpool_tool(api: &mut ExtensionApi, backend: Arc<dyn WorkpoolToolBackend>) {
    let attach = backend.clone();
    api.on(maho_ext_api::EventKind::SessionStart, Arc::new(move |_, context| {
        let backend = attach.clone();
        let session = context.session_manager.session_id().to_owned();
        Box::pin(async move {
            backend.attach(&session).await.map_err(|error| maho_ext_api::ExtensionFailure::new(error.to_string()))?;
            Ok(maho_ext_api::EventResult::None)
        })
    }));
    let tool = Arc::new(WorkpoolTool::new(backend));
    let definition_tool = tool.clone();
    let mut definition = ToolDefinition::new(
        tool.name,
        tool.description,
        tool.parameters.clone(),
        Arc::new(move |call| {
            let tool = definition_tool.clone();
            Box::pin(async move {
                let context = call.context.ok_or_else(|| {
                    ToolError::Message("workpool requires a session context".into())
                })?;
                let session_id = context.session_manager().session_id().to_owned();
                let caller = WorkpoolCaller {
                    session_id: session_id.clone(),
                    root_session_id: session_id,
                    depth: 0,
                    cwd: context.cwd().to_string_lossy().into_owned(),
                };
                let result = tool
                    .execute_json(&caller, call.params)
                    .await
                    .map_err(|error| ToolError::Message(error.to_string()))?;
                Ok(ToolResult {
                    content: vec![ToolContent::text(result.to_string())],
                    details: Some(result),
                    structured_content: None,
                    is_error: None,
                })
            })
        }),
    );
    definition.label = tool.label.into();
    api.register_tool(definition);
}
