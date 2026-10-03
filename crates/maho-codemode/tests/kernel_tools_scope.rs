use std::{collections::HashMap, sync::{Arc, Mutex}, time::Duration};
use maho_codemode::{kernels::js::context_manager::JavaScriptKernel, tool::{types::*, eval_tool_options::*, detached_cell_manager::EvalDetachedCellManager, image_resize::*, run_eval_cell::run_eval_cell}};
use maho_ext_api::{ExtensionKernelTools, KernelToolInvokeRequest, KernelToolInvokeOptions, AgentToolResult};
use maho_ext_host::kernel_tools_context::{current_kernel_tools, with_kernel_tools};
use serde_json::{Value, json};

struct Manager(Arc<JavaScriptKernel>);
impl EvalKernelManager for Manager {
    fn get_kernel(&self, _: EvalLanguage) -> EvalKernelFuture<'_, Arc<dyn EvalKernel>> { Box::pin(async { Ok(self.0.clone() as Arc<dyn EvalKernel>) }) }
}
struct Images;
impl EvalImageSdk for Images {
    fn resize_image<'a>(&'a self, _: Vec<u8>, _: &'a str, _: Option<usize>) -> ImageFuture<'a, Option<ResizedImage>> { Box::pin(async { panic!("text only") }) }
    fn convert_to_png<'a>(&'a self, _: &'a str, _: &'a str) -> ImageFuture<'a, Option<EvalImageContent>> { Box::pin(async { panic!("text only") }) }
}
struct Consumer(Mutex<Option<Arc<dyn ExtensionKernelTools>>>);
impl maho_codemode::bridges::output_bridge::OutputExecuteTool for Consumer {
    fn execute_tool<'a>(&'a self, _: &'a str, _: Value, _: maho_ext_api::ExecuteToolOptions) -> maho_ext_api::ExecuteToolFuture<'a> {
        Box::pin(async move {
            let tools=current_kernel_tools().expect("actual eval host call must carry capability");
            let described=tools.describe(&["increment".into()]).await.expect("consumer describes live tool");
            let result=tools.invoke(request(&described,"increment"),Default::default()).await.expect("consumer invokes live tool");
            *self.0.lock().expect("retained capability lock")=Some(tools);
            Ok(AgentToolResult::text(result.to_string()))
        })
    }
}
fn request(described: &Value, name: &str) -> KernelToolInvokeRequest {
    let descriptor=&described["results"][0]["descriptor"];
    KernelToolInvokeRequest { name:name.into(), kernel_generation:descriptor["kernel_generation"].as_u64().expect("descriptor generation"), definition_revision:descriptor["definition_revision"].as_u64().expect("descriptor revision"), args:json!({"value":41}), call_id:"scope-consumer".into() }
}
fn invocation(id: &str, code: &str) -> EvalCellInvocation {
    EvalCellInvocation {cell_id:id.into(), input:EvalToolInput {language:EvalLanguage::Js,code:code.into(),summary:"scope proof".into(),action:None,timeout:None,on_timeout:Some(TimeoutBehavior::Error),reset:None},signal:maho_ai::utils::abort::AbortController::new().signal(),on_update:None,mode:"print".into(),model:None}
}
async fn define(kernel: &JavaScriptKernel, code: &str) {
    let result=kernel.run(maho_codemode::kernels::shared::subprocess_contract::KernelRunInput {cell_id:"define".into(),code:code.into(),timeout_ms:Some(5000)}, |_|{}).await.expect("definition cell completes");
    assert_eq!(result["ok"],true,"{result}");
}

#[tokio::test]
async fn real_eval_consumer_retains_live_capability_and_nested_scope_restores_outer() {
    assert!(current_kernel_tools().is_none());
    let kernel=Arc::new(JavaScriptKernel::start(std::path::Path::new(env!("CARGO_MANIFEST_DIR")),"scope-consumer",4,None).await.unwrap());
    let consumer=Arc::new(Consumer(Mutex::new(None)));
    let options=Arc::new(CreateEvalToolOptions {kernel_manager:Arc::new(Manager(kernel.clone())),executor:consumer.clone(),list_tools:None,complete:None,settings:Default::default(),artifacts_dir:None,image_sdk:Arc::new(Images),cell_manager:Arc::new(Mutex::new(EvalDetachedCellManager::new(Default::default()))),on_cell_settled:None,prompt:Default::default(),runtimes:HashMap::new(),mode:"print".into()});
    let proof=tokio::time::timeout(Duration::from_secs(10),async {
        let result=run_eval_cell(options,invocation("consume","tool(function increment(value) { return value+1; }); display(await tool.consume({}));")).await.unwrap();
        assert_eq!(result.details["cells"][0]["status"],"complete","{result:?}");
        assert_eq!(result.details["jsonOutputs"][0]["text"],"42");
        let retained=consumer.0.lock().unwrap().clone().unwrap();
        let old=retained.describe(&["increment".into()]).await.unwrap();
        kernel.reset().await.unwrap();
        define(&kernel,"tool(function increment(value) { return value+2; })").await;
        let fresh=retained.describe(&["increment".into()]).await.unwrap();
        assert!(fresh["results"][0]["descriptor"]["kernel_generation"].as_u64().unwrap()>old["results"][0]["descriptor"]["kernel_generation"].as_u64().unwrap());
        assert!(retained.invoke(request(&old,"increment"),Default::default()).await.is_err());
        assert_eq!(retained.invoke(request(&fresh,"increment"),Default::default()).await.unwrap(),43);
        let outer:Arc<dyn ExtensionKernelTools>=Arc::new(OuterTools);
        with_kernel_tools(outer.clone(),async {
            with_kernel_tools(retained.clone(),async {assert!(Arc::ptr_eq(&current_kernel_tools().unwrap(),&retained));}).await;
            assert!(Arc::ptr_eq(&current_kernel_tools().unwrap(),&outer));
        }).await;
        assert!(current_kernel_tools().is_none());
    }).await;
    kernel.close().await.unwrap();
    assert!(kernel.pid().is_none());
    eprintln!("cleanup: scope-consumer worker closed; pid None");
    proof.unwrap();
}

struct OuterTools;
impl ExtensionKernelTools for OuterTools {
    fn invoke_scope(&self) -> bool { false }
    fn describe<'a>(&'a self, _: &'a [String]) -> maho_ext_api::ExtensionFuture<'a,Value> { Box::pin(async {Ok(json!({"outer":true}))}) }
    fn invoke(&self, _: KernelToolInvokeRequest, _: KernelToolInvokeOptions) -> maho_ext_api::ExtensionFuture<'_,Value> { Box::pin(async {panic!("outer capability must not be invoked")}) }
}

#[tokio::test]
async fn adapter_cancels_pending_worker_call_and_preserves_failure_message() {
    let kernel=Arc::new(JavaScriptKernel::start(std::path::Path::new(env!("CARGO_MANIFEST_DIR")),"scope-cancel",4,None).await.unwrap());
    let tools:Arc<dyn ExtensionKernelTools>=kernel.clone();
    let proof=tokio::time::timeout(Duration::from_secs(10),async {
        define(&kernel,"tool(async function parked() { return await tool.read({}); }); tool(function broken() { throw new Error('adapter-error-proof'); })").await;
        let parent=kernel.run_with_callbacks(maho_codemode::kernels::shared::subprocess_contract::KernelRunInput {cell_id:"cancel-parent".into(),code:"await tool.parent({})".into(),timeout_ms:Some(5000)},None,None);
        let nested=async {
        let parent_call=kernel.next_tool_call().await.unwrap();
        let described=tools.describe(&["parked".into()]).await.unwrap();
        let signal=maho_ext_api::AbortSignal::default();
        let call=tools.invoke(KernelToolInvokeRequest {args:json!({}),..request(&described,"parked")},KernelToolInvokeOptions {signal:Some(signal.clone()),scope:None});
        let cancel=async {let frame=kernel.next_tool_call().await.unwrap();assert_eq!(frame["toolName"],"read");signal.abort();};
        let (result,())=tokio::join!(call,cancel);
        assert!(result.unwrap_err().message.contains("cancel"));
        let described=tools.describe(&["broken".into()]).await.unwrap();
        assert!(tools.invoke(KernelToolInvokeRequest {args:json!({}),..request(&described,"broken")},Default::default()).await.unwrap_err().message.contains("adapter-error-proof"));
        kernel.deliver_tool_reply(json!({"type":"tool-reply","callId":parent_call["callId"],"ok":true,"value":null})).unwrap();
        };
        let (parent,())=tokio::join!(parent,nested);
        assert_eq!(parent.unwrap()["ok"],true);
    }).await;
    kernel.close().await.unwrap();
    assert!(kernel.pid().is_none());
    eprintln!("cleanup: scope-cancel worker closed; pid None");
    proof.unwrap();
}
