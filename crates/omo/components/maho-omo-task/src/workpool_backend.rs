use std::{future::Future, pin::Pin, sync::Arc};

use serde_json::{Value, json};
use senpi_task::{tools::{workpool::WorkpoolToolBackend, workpool_schema::{WorkpoolOperation, WorkpoolToolParams}}, workpool::{engine::WorkpoolEngine, types::{WorkpoolAgent, WorkpoolCaller, WorkpoolCreate, WorkpoolError, WorkpoolErrorCode, WorkpoolInput}}};

pub type KernelToolsResolver = Arc<dyn Fn(Vec<String>, senpi_task::workpool::types::WorkpoolSpec) -> Pin<Box<dyn Future<Output = Result<Vec<senpi_task::runners::in_process::shared_tool_filter::ChildToolRef>, senpi_task::host::HostError>> + Send>> + Send + Sync>;

pub struct NativeWorkpoolBackend {
    pub engine: Arc<tokio::sync::Mutex<WorkpoolEngine>>,
    pub resolve_kernel_tools: KernelToolsResolver,
}

impl WorkpoolToolBackend for NativeWorkpoolBackend {
    fn attach<'a>(&'a self, session_id: &'a str) -> Pin<Box<dyn Future<Output = Result<(), WorkpoolError>> + Send + 'a>> {
        Box::pin(async move { self.engine.lock().await.attach(session_id, None) })
    }
    fn execute<'a>(&'a self, caller: &'a WorkpoolCaller, input: WorkpoolToolParams) -> Pin<Box<dyn Future<Output = Result<Value, WorkpoolError>> + Send + 'a>> {
        Box::pin(async move {
            let mut engine = self.engine.lock().await;
            let invalid = || WorkpoolError::new(WorkpoolErrorCode::InvalidInput, "Invalid workpool arguments.");
            match input.op {
                WorkpoolOperation::Create => {
                    let agent = serde_json::from_value::<WorkpoolAgent>(serde_json::to_value(input.agent.ok_or_else(invalid)?).map_err(|_| invalid())?).map_err(|_| invalid())?;
                    let spec = engine.admission.resolve(caller, &agent).map_err(|error| WorkpoolError::new(WorkpoolErrorCode::AdmissionRefused, error.0))?;
                    let tools = (self.resolve_kernel_tools)(input.tools.clone().unwrap_or_default(), spec).await
                        .map_err(|error| WorkpoolError::new(WorkpoolErrorCode::ToolsUnavailable, error.message))?;
                    let pool = engine.create(caller, &WorkpoolCreate {
                        name: input.name.ok_or_else(invalid)?, agent,
                        mode: input.mode.map(|mode| match mode {
                            senpi_task::tools::workpool_schema::WorkpoolModeInput::Fresh => senpi_task::workpool::types::WorkpoolMode::Fresh,
                            senpi_task::tools::workpool_schema::WorkpoolModeInput::KeepAlive => senpi_task::workpool::types::WorkpoolMode::KeepAlive,
                        }), tools: input.tools,
                    })?;
                    engine.admission.bind_pool_kernel_tools(&pool.pool_id, tools)
                        .map_err(|error| WorkpoolError::new(WorkpoolErrorCode::ToolsUnavailable, error.0))?;
                    Ok(json!({"pool_id":pool.pool_id,"mode":pool.mode,"generation":pool.generation,"status":pool.status}))
                }
                WorkpoolOperation::Push => {
                    let items = input.items.ok_or_else(invalid)?.into_iter().map(|item| WorkpoolInput { key: item.key, input: item.input }).collect::<Vec<_>>();
                    engine.push(caller, input.pool_id.as_deref().ok_or_else(invalid)?, &items)
                }
                WorkpoolOperation::Close => {
                    let pool = engine.close(caller, input.pool_id.as_deref().ok_or_else(invalid)?)?;
                    Ok(json!({"pool_id":pool.pool_id,"status":pool.status,"generation":pool.generation}))
                }
                WorkpoolOperation::Inspect => {
                    let pool = engine.inspect(caller, input.pool_id.as_deref().ok_or_else(invalid)?)?;
                    Ok(json!({"pool_id":pool.pool_id,"name":pool.name,"mode":pool.mode,"status":pool.status,"generation":pool.generation,"revision":pool.revision,"items":pool.items,"workers":pool.workers,"aggregate":pool.aggregate}))
                }
                WorkpoolOperation::Cancel => {
                    let pool = engine.cancel(caller, input.pool_id.as_deref().ok_or_else(invalid)?).await?;
                    Ok(json!({"pool_id":pool.pool_id,"status":pool.status,"generation":pool.generation}))
                }
                WorkpoolOperation::Yield => Err(WorkpoolError::new(WorkpoolErrorCode::ScopeDenied, "Worker yield requires an assigned worker capability.")),
            }
        })
    }
}
