use std::sync::Arc;

use maho_ext_api::{ExtensionKernelTools, KernelToolInvokeOptions, KernelToolInvokeRequest};
use serde_json::{Value, json};
use senpi_task::host::HostError;
use senpi_task::{
    kernel_tools::{KernelToolGrantRequest, KernelToolGrantResolution, KernelToolsCapability, resolve_kernel_tool_grant},
    manager::execution_mode::ExecutionMode,
    runners::in_process::shared_tool_filter::{ChildTool, ChildToolRef},
};

pub async fn capture_requested_tools(
    names: Vec<String>,
    execution_mode: ExecutionMode,
    agent_type: Option<String>,
    allow: Option<Vec<String>>,
    deny: Option<Vec<String>>,
    existing_names: Vec<String>,
) -> Result<Vec<ChildToolRef>, HostError> {
    if names.is_empty() { return Ok(Vec::new()); }
    let capability = maho_ext_api::current_kernel_tools().ok_or_else(|| HostError {
        message: "Kernel tools require a live JavaScript worker context; this parent invocation has none.".into(),
    })?;
    let described = capability.describe(&names).await.map_err(|error| HostError { message: error.to_string() })?;
    let request = KernelToolGrantRequest {
        requested_names: names,
        capability: Some(KernelToolsCapability {
            capabilities: Some(senpi_task::kernel_tools::KernelToolCapabilities { invoke_scope: capability.invoke_scope() }),
        }),
        execution_mode,
        agent_type,
        team_role_member: false,
        existing_tool_names: existing_names,
        tool_allowlist: allow,
        tool_denylist: deny,
    };
    match resolve_kernel_tool_grant(&request, Some(&described)) {
        KernelToolGrantResolution::None => Ok(Vec::new()),
        KernelToolGrantResolution::Denied { code, message } => Err(HostError { message: format!("{}: {message}", code.as_str()) }),
        KernelToolGrantResolution::Granted(grant) => Ok(grant.descriptors.into_iter().map(|descriptor| {
            Arc::new(KernelChildTool::new(descriptor, capability.clone(), grant.scope.clone())) as ChildToolRef
        }).collect()),
    }
}

pub async fn capture_pool_tools(
    names: Vec<String>,
    spec: &senpi_task::workpool::types::WorkpoolSpec,
) -> Result<Vec<ChildToolRef>, HostError> {
    let execution_mode = spec.start.execution_mode
        .or(spec.plan.agent_execution_mode)
        .unwrap_or(ExecutionMode::InProcess);
    let agent_type = spec.plan.agent_type.clone().or_else(|| spec.start.subagent_type.clone());
    let existing_names = spec.start.member_scoped_tools.as_deref().unwrap_or_default()
        .iter().map(|tool| tool.name().to_owned()).collect();
    capture_requested_tools(
        names,
        execution_mode,
        agent_type,
        spec.plan.tool_allowlist.clone(),
        spec.plan.tool_denylist.clone(),
        existing_names,
    ).await
}

pub struct KernelChildTool {
    descriptor: senpi_task::kernel_tools::KernelToolDescriptor,
    capability: Arc<dyn ExtensionKernelTools>,
    scope: Option<senpi_task::kernel_tools::KernelToolInvokeScope>,
}

impl KernelChildTool {
    pub fn new(
        descriptor: senpi_task::kernel_tools::KernelToolDescriptor,
        capability: Arc<dyn ExtensionKernelTools>,
        scope: Option<senpi_task::kernel_tools::KernelToolInvokeScope>,
    ) -> Self {
        Self { descriptor, capability, scope }
    }
}

impl ChildTool for KernelChildTool {
    fn name(&self) -> &str { &self.descriptor.name }

    fn description(&self) -> &str { &self.descriptor.description }

    fn parameters(&self) -> Option<Value> { Some(Value::Object(self.descriptor.input_schema.clone())) }

    fn execute(&self, _: &str, _: &Value) -> Result<Value, HostError> {
        Err(HostError { message: "Parent kernel tools require asynchronous child execution".into() })
    }

    fn execute_async<'a>(
        &'a self,
        tool_call_id: &'a str,
        input: &'a Value,
        signal: Option<maho_ai::utils::abort::AbortSignal>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, HostError>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(signal) = signal.as_ref() {
                signal.throw_if_aborted().map_err(|error| HostError { message: error.to_string() })?;
            }
            let extension_signal = maho_ext_api::AbortSignal::default();
            let cancellation = signal.as_ref().map(|signal| {
                let signal = signal.clone();
                let extension_signal = extension_signal.clone();
                tokio::spawn(async move {
                    signal.cancelled().await;
                    extension_signal.abort();
                })
            });
            let invoke = self.capability.invoke(KernelToolInvokeRequest {
                name: self.descriptor.name.clone(),
                kernel_generation: self.descriptor.kernel_generation as u64,
                definition_revision: self.descriptor.definition_revision as u64,
                args: input.clone(),
                call_id: tool_call_id.into(),
            }, KernelToolInvokeOptions {
                signal: cancellation.as_ref().map(|_| extension_signal.clone()),
                scope: self.scope.as_ref().and_then(|scope| scope.tools.as_ref()).map(|scope| {
                    maho_ext_api::KernelToolInvokeScope { allow: scope.allow.clone(), deny: scope.deny.clone() }
                }),
            });
            let value = if let Some(signal) = signal.as_ref() {
                maho_ai::utils::abort::race_with_abort_signal(invoke, signal).await
                    .map_err(|error| HostError { message: error.to_string() })
                    .and_then(|result| result.map_err(|error| HostError { message: error.to_string() }))
            } else {
                invoke.await.map_err(|error| HostError { message: error.to_string() })
            };
            if signal.as_ref().is_some_and(|signal| signal.aborted()) { extension_signal.abort(); }
            if let Some(cancellation) = cancellation { cancellation.abort(); }
            let value = value?;
            Ok(kernel_tool_result(value))
        })
    }
}

fn kernel_tool_result(value: Value) -> Value {
    let content = value.get("content").and_then(Value::as_array).cloned().unwrap_or_else(|| {
        vec![json!({"type":"text","text":value.as_str().map(str::to_owned).unwrap_or_else(||value.to_string())})]
    });
    let mut result = json!({"content":content});
    if let Some(details) = value.get("details") { result["details"] = details.clone(); }
    if value["isError"] == true { result["isError"] = Value::Bool(true); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::{ExtensionFuture, KernelToolInvokeOptions};
    use serde_json::Map;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Fixture { requests: Mutex<Vec<(KernelToolInvokeRequest, KernelToolInvokeOptions)>> }

    fn described(names: &[String]) -> Value {
        json!({"results": names.iter().map(|name| json!({
            "name":name,"ok":true,"descriptor":{
                "name":name,"description":"fixture","input_schema":{"type":"object","properties":{"q":{"type":"string"}}},
                "language":"js","kernel_generation":8,"definition_revision":2
            }
        })).collect::<Vec<_>>()})
    }

    impl ExtensionKernelTools for Fixture {
        fn invoke_scope(&self) -> bool { true }
        fn describe<'a>(&'a self, names: &'a [String]) -> ExtensionFuture<'a, Value> {
            Box::pin(async move { Ok(described(names)) })
        }
        fn invoke(&self, request: KernelToolInvokeRequest, options: KernelToolInvokeOptions) -> ExtensionFuture<'_, Value> {
            self.requests.lock().expect("fixture lock").push((request, options));
            Box::pin(async { Ok(json!({"content":[{"type":"text","text":"ok"}],"details":{"done":true}})) })
        }
    }

    #[tokio::test]
    async fn invokes_captured_kernel_with_descriptor_scope_and_signal() {
        struct PendingFixture {
            requests: std::sync::Mutex<Vec<(KernelToolInvokeRequest, KernelToolInvokeOptions)>>,
            invoked: tokio::sync::Notify,
        }
        impl ExtensionKernelTools for PendingFixture {
            fn invoke_scope(&self) -> bool { true }
            fn describe<'a>(&'a self, names: &'a [String]) -> ExtensionFuture<'a, Value> {
                Box::pin(async move { Ok(described(names)) })
            }
            fn invoke(&self, request: KernelToolInvokeRequest, options: KernelToolInvokeOptions) -> ExtensionFuture<'_, Value> {
                let signal = options.signal.clone().expect("child cancellation signal");
                self.requests.lock().expect("fixture lock").push((request, options));
                Box::pin(async move {
                    self.invoked.notify_one();
                    signal.cancelled().await;
                    Err(maho_ext_api::ExtensionFailure::new("cancelled fixture"))
                })
            }
        }
        let fixture = Arc::new(PendingFixture { requests: Default::default(), invoked: tokio::sync::Notify::new() });
        let capability: Arc<dyn ExtensionKernelTools> = fixture.clone();
        let controller = maho_ai::utils::abort::AbortController::new();
        let signal = controller.signal();
        let tool = KernelChildTool::new(
            senpi_task::kernel_tools::KernelToolDescriptor {
                name: "lookup".into(), description: "lookup".into(), input_schema: Map::new(),
                language: senpi_task::kernel_tools::KernelToolLanguage::Js,
                kernel_generation: 8, definition_revision: 2,
            }, capability.clone(), Some(senpi_task::kernel_tools::KernelToolInvokeScope {
                tools: Some(senpi_task::kernel_tools::KernelToolHostScope {
                    allow: Some(vec!["read".into()]), deny: Some(vec!["write".into()]),
                }),
            }),
        );
        let invoked = fixture.invoked.notified();
        let input = json!({"q":"x"});
        let execution = tool.execute_async("child-call", &input, Some(signal.clone()));
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(execution, async { invoked.await; controller.abort(None); })
        }).await.expect("bounded cancellation");
        assert!(result.is_err());
        let (request, options) = fixture.requests.lock().expect("fixture lock").pop().expect("invoke");
        assert_eq!(request.kernel_generation, 8);
        assert_eq!(request.definition_revision, 2);
        assert_eq!(request.call_id, "child-call");
        assert_eq!(options.scope.unwrap().allow, Some(vec!["read".into()]));
        let extension_signal = options.signal.unwrap();
        assert!(extension_signal.is_aborted());
        assert!(Arc::strong_count(&capability) > 1);
    }

    #[tokio::test]
    async fn aborted_call_does_not_reach_kernel() {
        let fixture = Arc::new(Fixture::default());
        let capability: Arc<dyn ExtensionKernelTools> = fixture.clone();
        let controller = maho_ai::utils::abort::AbortController::new();
        controller.abort(None);
        let tool = KernelChildTool::new(
            senpi_task::kernel_tools::KernelToolDescriptor {
                name: "lookup".into(), description: String::new(), input_schema: Map::new(),
                language: senpi_task::kernel_tools::KernelToolLanguage::Js,
                kernel_generation: 1, definition_revision: 1,
            }, capability, None,
        );
        assert!(tool.execute_async("call", &json!({}), Some(controller.signal())).await.is_err());
        assert!(fixture.requests.lock().expect("fixture lock").is_empty());
    }

    #[tokio::test]
    async fn captures_requested_descriptors_from_task_local_capability() {
        let fixture = Arc::new(Fixture::default());
        let capability: Arc<dyn ExtensionKernelTools> = fixture.clone();
        let names = vec!["lookup".to_owned()];
        let tools = maho_ext_api::with_kernel_tools(Some(capability.clone()), capture_requested_tools(
            names, ExecutionMode::InProcess, None, Some(vec!["read".into()]), Some(vec!["write".into()]), vec![],
        )).await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name(), "lookup");
        assert_eq!(tools[0].parameters().unwrap()["properties"]["q"]["type"], "string");
        let signal = maho_ai::utils::abort::AbortController::new().signal();
        tools[0].execute_async("child-id", &json!({"q":"yes"}), Some(signal)).await.unwrap();
        let (request, options) = fixture.requests.lock().expect("fixture lock").pop().expect("invoke");
        assert_eq!(request.name, "lookup");
        assert_eq!(request.kernel_generation, 8);
        assert_eq!(request.definition_revision, 2);
        assert_eq!(options.scope.unwrap().deny, Some(vec!["write".into()]));
        assert!(!options.signal.unwrap().is_aborted());
    }

    #[tokio::test]
    async fn missing_task_local_capability_fails_closed() {
        let error = capture_requested_tools(vec!["lookup".into()], ExecutionMode::InProcess, None, None, None, vec![])
            .await.err().expect("missing task-local capability must fail");
        assert!(error.message.contains("live JavaScript worker context"));
    }

    #[tokio::test]
    async fn resolver_refuses_curated_agent_grants() {
        let fixture = Arc::new(Fixture::default());
        let capability: Arc<dyn ExtensionKernelTools> = fixture;
        let result = maho_ext_api::with_kernel_tools(Some(capability), capture_requested_tools(
            vec!["lookup".into()], ExecutionMode::InProcess, Some("explore".into()), None, None, vec![],
        )).await;
        assert!(result.err().expect("curated grant must be denied").message.contains("curated_policy_denied"));
    }
}
