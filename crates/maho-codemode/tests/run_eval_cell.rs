use std::{collections::HashMap,sync::{Arc,Mutex}};
use serde_json::json;
use maho_codemode::{tool::{eval_tool_options::*,types::*,image_resize::*,detached_cell_manager::*,run_eval_cell::run_eval_cell},kernels::py::{kernel::PythonKernel,kernel_contract::PythonKernelStartOptions},bridge::protocol::BridgeConnectionConfig};

struct Manager(Arc<PythonKernel>);
impl EvalKernelManager for Manager {
    fn get_kernel(&self,language:EvalLanguage)->EvalKernelFuture<'_,Arc<dyn EvalKernel>> {
        Box::pin(async move {assert_eq!(language,EvalLanguage::Py);Ok(self.0.clone() as Arc<dyn EvalKernel>)})
    }
}
struct JsManager(Arc<maho_codemode::kernels::js::context_manager::JavaScriptKernel>);
struct AcquiringManager(tokio::sync::mpsc::UnboundedSender<()>);
impl EvalKernelManager for AcquiringManager {
    fn get_kernel(&self,_:EvalLanguage)->EvalKernelFuture<'_,Arc<dyn EvalKernel>> {
        Box::pin(async {self.0.send(()).expect("acquisition event receiver");std::future::pending().await})
    }
}
impl EvalKernelManager for JsManager {
    fn get_kernel(&self,language:EvalLanguage)->EvalKernelFuture<'_,Arc<dyn EvalKernel>> {Box::pin(async move {assert_eq!(language,EvalLanguage::Js);Ok(self.0.clone() as Arc<dyn EvalKernel>)})}
}
struct Images;
impl EvalImageSdk for Images {
    fn resize_image<'a>(&'a self,_:Vec<u8>,_:&'a str,_:Option<usize>)->ImageFuture<'a,Option<ResizedImage>> {Box::pin(async {panic!("text-only eval must not resize images")})}
    fn convert_to_png<'a>(&'a self,_:&'a str,_:&'a str)->ImageFuture<'a,Option<EvalImageContent>> {Box::pin(async {panic!("text-only eval must not convert images")})}
}
struct Executor;
impl maho_codemode::bridges::output_bridge::OutputExecuteTool for Executor {
    fn execute_tool<'a>(&'a self,_:&'a str,_:serde_json::Value,_:maho_ext_api::ExecuteToolOptions)->maho_ext_api::ExecuteToolFuture<'a> {Box::pin(async {panic!("text-only eval must not call host tools")})}
}
async fn fixture()->(Arc<PythonKernel>,Arc<CreateEvalToolOptions>) {
    let kernel=Arc::new(PythonKernel::start(PythonKernelStartOptions {interpreter_path:"python3".into(),session_id:"eval-chain".into(),cwd:env!("CARGO_MANIFEST_DIR").into(),connection:BridgeConnectionConfig {port:1,token:"test".into(),local_roots:None,artifacts_dir:None,parallel_pool_width:None},env:None,session_env:None,startup_timeout:None,on_message:None}).await.expect("Python fixture startup"));
    let options=Arc::new(CreateEvalToolOptions {kernel_manager:Arc::new(Manager(kernel.clone())),executor:Arc::new(Executor),list_tools:None,complete:None,settings:Default::default(),artifacts_dir:None,image_sdk:Arc::new(Images),cell_manager:Arc::new(Mutex::new(EvalDetachedCellManager::new(Default::default()))),on_cell_settled:None,prompt:Default::default(),runtimes:HashMap::new(),mode:"print".into()});
    (kernel,options)
}
fn invocation(id:&str,code:&str)->EvalCellInvocation {
    EvalCellInvocation {cell_id:id.into(),input:EvalToolInput {language:EvalLanguage::Py,code:code.into(),summary:"compute a value".into(),action:None,timeout:None,on_timeout:Some(TimeoutBehavior::Error),reset:None},signal:maho_ai::utils::abort::AbortController::new().signal(),on_update:None,mode:"print".into()}
}

struct FinalFrameKernel;
impl EvalKernel for FinalFrameKernel {
    fn run(&self,input:EvalKernelRunInput)->EvalKernelFuture<'_,serde_json::Value> {Box::pin(async move {
        if let Some(started)=input.on_started {started();}
        if let Some(message)=input.on_message {message(&json!({"type":"unsupported-final-frame"}));}
        Ok(json!({"type":"result","cellId":input.cell_id,"ok":true,"valueRepr":"42","durationMs":0}))
    })}
    fn cancel_queued<'a>(&'a self,_:&'a str,_:&'a str)->EvalKernelFuture<'a,bool> {Box::pin(async {Ok(false)})}
    fn interrupt<'a>(&'a self,_:&'a str,_:Option<&'a str>)->EvalKernelFuture<'a,KernelInterruptHandle> {Box::pin(async {Ok(KernelInterruptHandle {state_retained:Box::pin(async {Ok(true)}),note:None})})}
    fn queue_snapshot(&self)->(Option<String>,Vec<String>) {(None,vec![])}
    fn deliver_tool_reply(&self,_:serde_json::Value)->Result<(),String> {Ok(())}
    fn reset(&self)->EvalKernelFuture<'_,()> {Box::pin(async {Ok(())})}
    fn close(&self)->EvalKernelFuture<'_,()> {Box::pin(async {Ok(())})}
}
struct FinalFrameManager;
impl EvalKernelManager for FinalFrameManager {
    fn get_kernel(&self,_:EvalLanguage)->EvalKernelFuture<'_,Arc<dyn EvalKernel>> {Box::pin(async {Ok(Arc::new(FinalFrameKernel) as Arc<dyn EvalKernel>)})}
}

struct ParkedToolKernel;
impl EvalKernel for ParkedToolKernel {
    fn run(&self,input:EvalKernelRunInput)->EvalKernelFuture<'_,serde_json::Value> {Box::pin(async move {
        input.on_started.expect("start callback")();
        input.on_message.expect("message callback")(&json!({"type":"tool-call","toolName":"park","callId":"parked","args":{}}));
        std::future::pending().await
    })}
    fn cancel_queued<'a>(&'a self,_:&'a str,_:&'a str)->EvalKernelFuture<'a,bool> {Box::pin(async {Ok(false)})}
    fn interrupt<'a>(&'a self,_:&'a str,_:Option<&'a str>)->EvalKernelFuture<'a,KernelInterruptHandle> {Box::pin(async {Ok(KernelInterruptHandle {state_retained:Box::pin(async {Ok(true)}),note:None})})}
    fn queue_snapshot(&self)->(Option<String>,Vec<String>) {(None,vec![])}
    fn deliver_tool_reply(&self,_:serde_json::Value)->Result<(),String> {panic!("cancelled tool must not publish reply")}
    fn reset(&self)->EvalKernelFuture<'_,()> {Box::pin(async {Ok(())})}
    fn close(&self)->EvalKernelFuture<'_,()> {Box::pin(async {Ok(())})}
}
struct ParkedToolManager;
impl EvalKernelManager for ParkedToolManager {
    fn get_kernel(&self,_:EvalLanguage)->EvalKernelFuture<'_,Arc<dyn EvalKernel>> {Box::pin(async {Ok(Arc::new(ParkedToolKernel) as Arc<dyn EvalKernel>)})}
}
struct ParkedExecutor(tokio::sync::mpsc::UnboundedSender<()>);
impl maho_codemode::bridges::output_bridge::OutputExecuteTool for ParkedExecutor {
    fn execute_tool<'a>(&'a self,_:&'a str,_:serde_json::Value,_:maho_ext_api::ExecuteToolOptions)->maho_ext_api::ExecuteToolFuture<'a> {Box::pin(async {self.0.send(()).expect("tool start receiver");std::future::pending().await})}
}

struct ConcurrentExecutor(tokio::sync::Notify);
impl maho_codemode::bridges::output_bridge::OutputExecuteTool for ConcurrentExecutor {
    fn execute_tool<'a>(&'a self,name:&'a str,_:serde_json::Value,_:maho_ext_api::ExecuteToolOptions)->maho_ext_api::ExecuteToolFuture<'a> {
        Box::pin(async move {
            if name=="first" {
                tokio::time::timeout(std::time::Duration::from_secs(2),self.0.notified()).await.map_err(|_|maho_ext_api::ExecuteToolError {code:maho_ext_api::ExecuteToolErrorCode::Blocked,tool_name:name.into(),message:"second host call never admitted".into(),active_tools:vec![]})?;
            } else {self.0.notify_one();}
            Ok(maho_ext_api::AgentToolResult::text(name))
        })
    }
}

#[tokio::test]
async fn real_js_parallel_host_calls_admit_while_first_is_pending() {
    let kernel=Arc::new(maho_codemode::kernels::js::context_manager::JavaScriptKernel::start(std::path::Path::new(env!("CARGO_MANIFEST_DIR")),"eval-parallel-host",4,None).await.unwrap());
    let options=Arc::new(CreateEvalToolOptions {kernel_manager:Arc::new(JsManager(kernel.clone())),executor:Arc::new(ConcurrentExecutor(tokio::sync::Notify::new())),list_tools:None,complete:None,settings:Default::default(),artifacts_dir:None,image_sdk:Arc::new(Images),cell_manager:Arc::new(Mutex::new(EvalDetachedCellManager::new(Default::default()))),on_cell_settled:None,prompt:Default::default(),runtimes:HashMap::new(),mode:"print".into()});
    let mut input=invocation("parallel-host","await Promise.all([tool.first({}),tool.second({})]); 42");
    input.input.language=EvalLanguage::Js;
    let result=run_eval_cell(options,input).await;
    kernel.close().await.unwrap();
    let result=result.unwrap();
    assert_ne!(result.details["isError"],true,"parallel calls must not serialize admission: {result:?}");
    assert_eq!(result.details["toolCalls"].as_array().unwrap().len(),2);
}

#[tokio::test]
async fn successful_final_frame_waits_for_unawaited_parallel_host_calls() {
    let kernel=Arc::new(maho_codemode::kernels::js::context_manager::JavaScriptKernel::start(std::path::Path::new(env!("CARGO_MANIFEST_DIR")),"eval-unawaited-host",4,None).await.unwrap());
    let options=Arc::new(CreateEvalToolOptions {kernel_manager:Arc::new(JsManager(kernel.clone())),executor:Arc::new(ConcurrentExecutor(tokio::sync::Notify::new())),list_tools:None,complete:None,settings:Default::default(),artifacts_dir:None,image_sdk:Arc::new(Images),cell_manager:Arc::new(Mutex::new(EvalDetachedCellManager::new(Default::default()))),on_cell_settled:None,prompt:Default::default(),runtimes:HashMap::new(),mode:"print".into()});
    let mut input=invocation("unawaited-host","void tool.first({}); void tool.second({}); 42");
    input.input.language=EvalLanguage::Js;
    let result=run_eval_cell(options,input).await;
    kernel.close().await.unwrap();
    let result=result.unwrap();
    assert_ne!(result.details["isError"],true,"unawaited calls must settle before success: {result:?}");
    let calls=result.details["toolCalls"].as_array().unwrap();
    assert_eq!(calls.len(),2);
    assert!(calls.iter().all(|call|call["ok"]==true));
}

#[tokio::test]
async fn caller_abort_settles_while_host_tool_remains_pending() {
    let (started,mut events)=tokio::sync::mpsc::unbounded_channel();
    let options=Arc::new(CreateEvalToolOptions {kernel_manager:Arc::new(ParkedToolManager),executor:Arc::new(ParkedExecutor(started)),list_tools:None,complete:None,settings:Default::default(),artifacts_dir:None,image_sdk:Arc::new(Images),cell_manager:Arc::new(Mutex::new(EvalDetachedCellManager::new(Default::default()))),on_cell_settled:None,prompt:Default::default(),runtimes:HashMap::new(),mode:"print".into()});
    let controller=maho_ai::utils::abort::AbortController::new();
    let mut input=invocation("parked","await tool.park()");input.signal=controller.signal();
    let run=tokio::spawn(run_eval_cell(options,input));
    tokio::time::timeout(std::time::Duration::from_secs(2),events.recv()).await.unwrap().unwrap();
    controller.abort(Some(maho_ai::utils::abort::AbortReason::new("AbortError","caller cancelled")));
    let result=tokio::time::timeout(std::time::Duration::from_secs(2),run).await.expect("caller abort must not wait for host tool").unwrap().unwrap();
    assert_eq!(result.details["isError"],true);
}

#[tokio::test]
async fn invalid_final_frame_cannot_settle_as_success() {
    let options=Arc::new(CreateEvalToolOptions {kernel_manager:Arc::new(FinalFrameManager),executor:Arc::new(Executor),list_tools:None,complete:None,settings:Default::default(),artifacts_dir:None,image_sdk:Arc::new(Images),cell_manager:Arc::new(Mutex::new(EvalDetachedCellManager::new(Default::default()))),on_cell_settled:None,prompt:Default::default(),runtimes:HashMap::new(),mode:"print".into()});
    let result=run_eval_cell(options.clone(),invocation("invalid-final","42")).await.unwrap();
    assert_eq!(result.details["isError"],true);
    assert_ne!(result.details["cells"][0]["status"],"complete");
    assert!(result.details["cells"][0]["output"].as_str().unwrap().contains("Unhandled kernel message"));
}

#[tokio::test(start_paused = true)]
async fn deadline_expiry_records_limit_metadata_before_terminal_snapshot() {
    for (hard,budget) in [(1.0,10.0),(10.0,1.0)] {
        let (started,mut events)=tokio::sync::mpsc::unbounded_channel();
        let cells=Arc::new(Mutex::new(EvalDetachedCellManager::new(DetachedCellManagerOptions {hard_limit_seconds:hard,run_budget_seconds:budget,..Default::default()})));
        let options=Arc::new(CreateEvalToolOptions {kernel_manager:Arc::new(ParkedToolManager),executor:Arc::new(ParkedExecutor(started)),list_tools:None,complete:None,settings:Default::default(),artifacts_dir:None,image_sdk:Arc::new(Images),cell_manager:cells.clone(),on_cell_settled:None,prompt:Default::default(),runtimes:HashMap::new(),mode:"print".into()});
        let run=tokio::spawn(run_eval_cell(options,invocation("expired","await tool.park()")));
        events.recv().await.unwrap();
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        let result=run.await.unwrap().unwrap();
        assert_eq!(result.details["isError"],true);
        let snapshot=cells.lock().unwrap().peek("expired").unwrap();
        assert_eq!(snapshot.hard_limit_seconds,(hard==1.0).then_some(hard));
        assert_eq!(snapshot.run_budget_seconds,(budget==1.0).then_some(budget));
    }
}

#[tokio::test(start_paused = true)]
async fn acquisition_hard_deadline_settles_without_kernel_and_records_limit() {
    let (started,mut events)=tokio::sync::mpsc::unbounded_channel();
    let cells=Arc::new(Mutex::new(EvalDetachedCellManager::new(DetachedCellManagerOptions {hard_limit_seconds:1.0,..Default::default()})));
    let options=Arc::new(CreateEvalToolOptions {kernel_manager:Arc::new(AcquiringManager(started)),executor:Arc::new(Executor),list_tools:None,complete:None,settings:Default::default(),artifacts_dir:None,image_sdk:Arc::new(Images),cell_manager:cells.clone(),on_cell_settled:None,prompt:Default::default(),runtimes:HashMap::new(),mode:"print".into()});
    let run=tokio::spawn(run_eval_cell(options,invocation("boot-deadline","42")));
    events.recv().await.unwrap();
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    assert!(run.await.unwrap().is_err());
    let snapshot=cells.lock().unwrap().peek("boot-deadline").unwrap();
    assert_eq!(snapshot.hard_limit_seconds,Some(1.0));
    assert_eq!(snapshot.run_budget_seconds,None);
}

#[tokio::test]
async fn real_eval_chain_preserves_output_state_and_terminal_snapshot() {
    let (kernel,options)=fixture().await;
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),run_eval_cell(options.clone(),invocation("first","value=41\nprint(value)\nvalue+1"))).await.unwrap().unwrap();
    assert_eq!(result.details["cells"][0]["status"],"complete");
    assert!(result.details["cells"][0]["output"].as_str().unwrap().contains("41"));
    assert!(result.details["cells"][0]["output"].as_str().unwrap().ends_with("42"));
    let second=run_eval_cell(options.clone(),invocation("second","value+2")).await.unwrap();
    assert_eq!(second.details["cells"][0]["output"],"43");
    let snapshot=options.cell_manager.lock().unwrap().peek("second").unwrap();
    assert_eq!(snapshot.result.details["cells"][0]["output"],"43");
    assert_eq!(snapshot.result.details["isError"],json!(null));
    kernel.close().await.unwrap();
}

#[tokio::test]
async fn ordinary_eval_does_not_read_the_host_catalog() {
    let (kernel,options)=fixture().await;
    let mut options=Arc::try_unwrap(options).ok().expect("fixture options ownership");
    let calls=Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count=calls.clone();
    options.list_tools=Some(Arc::new(move || {count.fetch_add(1,std::sync::atomic::Ordering::SeqCst);Ok(vec![])}));
    let result=run_eval_cell(Arc::new(options),invocation("no-catalog","6*7")).await;
    kernel.close().await.unwrap();
    assert_eq!(result.unwrap().details["cells"][0]["output"],"42");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst),0,"source reads the catalog only when dispatch needs it");
}

#[tokio::test]
async fn real_js_eval_entry_preserves_state_and_delivers_host_replies() {
    let kernel=Arc::new(maho_codemode::kernels::js::context_manager::JavaScriptKernel::start(std::path::Path::new(env!("CARGO_MANIFEST_DIR")),"js-eval",4,None).await.unwrap());
    let options=Arc::new(CreateEvalToolOptions {kernel_manager:Arc::new(JsManager(kernel.clone())),executor:Arc::new(Executor),list_tools:None,complete:None,settings:Default::default(),artifacts_dir:None,image_sdk:Arc::new(Images),cell_manager:Arc::new(Mutex::new(EvalDetachedCellManager::new(Default::default()))),on_cell_settled:None,prompt:Default::default(),runtimes:HashMap::new(),mode:"print".into()});
    let mut first=invocation("js-first","var retained=41; print(retained); retained+1");first.input.language=EvalLanguage::Js;
    let first=run_eval_cell(options.clone(),first).await;
    let mut next=invocation("js-next","retained+2");next.input.language=EvalLanguage::Js;
    let next=run_eval_cell(options.clone(),next).await;
    let mut recursive=invocation("js-recursive","await tool.eval({})");recursive.input.language=EvalLanguage::Js;
    let recursive=run_eval_cell(options,recursive).await;
    kernel.close().await.unwrap();
    assert!(first.unwrap().details["cells"][0]["output"].as_str().unwrap().contains("41"));
    assert_eq!(next.unwrap().details["cells"][0]["output"],"43");
    assert_eq!(recursive.unwrap().details["isError"],true);
}

#[tokio::test]
async fn registered_eval_tool_runs_and_lists_real_kernel_results() {
    use maho_tools::definition::{ToolCall,AbortSignal};
    let (kernel,options)=fixture().await;
    let definition=maho_codemode::tool::eval_tool::create_eval_tool(options).unwrap();
    let result=(definition.execute)(ToolCall {id:"registered",params:json!({"language":"py","code":"6*7","summary":"compute answer","on_timeout":"error"}),signal:AbortSignal::default(),on_update:None,context:None}).await.unwrap();
    assert_eq!(result.details.unwrap()["cells"][0]["output"],"42");
    let result=(definition.execute)(ToolCall {id:"list",params:json!({"action":"list"}),signal:AbortSignal::default(),on_update:None,context:None}).await.unwrap();
    assert_eq!(result.details.unwrap()["cells"][0]["cellId"],"registered");
    let result=(definition.execute)(ToolCall {id:"peek",params:json!({"action":"peek","cell_id":"registered"}),signal:AbortSignal::default(),on_update:None,context:None}).await.unwrap();
    assert_eq!(result.details.unwrap()["cells"][0]["output"],"42");
    kernel.close().await.unwrap();
}

#[tokio::test]
async fn detached_real_eval_can_be_stopped_without_losing_python_state() {
    let (kernel,options)=fixture().await;
    let mut options=Arc::try_unwrap(options).ok().expect("fixture options ownership");
    options.settings.cell_timeout_seconds=0.02;
    options.settings.foreground_window_seconds=0.06;
    let options=Arc::new(options);
    let mut input=invocation("detached","value=41\nwhile True: pass");
    input.input.on_timeout=Some(TimeoutBehavior::Detach);
    input.mode="interactive".into();
    let result=tokio::time::timeout(std::time::Duration::from_secs(5),run_eval_cell(options.clone(),input)).await.unwrap().unwrap();
    assert_eq!(result.details["cells"][0]["status"],"detached");
    let snapshot=tokio::time::timeout(std::time::Duration::from_secs(7),EvalDetachedCellManager::stop(&options.cell_manager,"detached","test stop")).await.unwrap().unwrap();
    assert_eq!(snapshot.state,maho_codemode::tool::detached_cell_contract::EvalDetachedCellState::Cancelled);
    assert_eq!(snapshot.state_retained,Some(true));
    let result=run_eval_cell(options,invocation("after-stop","value+1")).await.unwrap();
    assert_eq!(result.details["cells"][0]["output"],"42");
    kernel.close().await.unwrap();
}

#[tokio::test]
async fn disposal_stops_real_detached_python_and_preserves_kernel_state() {
    let (kernel,options)=fixture().await;
    let mut options=Arc::try_unwrap(options).ok().expect("fixture options ownership");
    options.settings.cell_timeout_seconds=0.02;
    options.settings.foreground_window_seconds=0.06;
    let options=Arc::new(options);
    let mut input=invocation("dispose-detached","value=41\nwhile True: pass");
    input.input.on_timeout=Some(TimeoutBehavior::Detach);input.mode="interactive".into();
    let detached=tokio::time::timeout(std::time::Duration::from_secs(5),run_eval_cell(options.clone(),input)).await.unwrap().unwrap();
    let terminal=options.cell_manager.lock().unwrap().terminal_signal("dispose-detached").unwrap();
    let disposed=tokio::time::timeout(std::time::Duration::from_secs(7),EvalDetachedCellManager::dispose(&options.cell_manager)).await;
    let recovered=run_eval_cell(options.clone(),invocation("after-dispose","value+1")).await;
    kernel.close().await.unwrap();
    assert_eq!(detached.details["cells"][0]["status"],"detached");
    disposed.unwrap().unwrap();
    assert_eq!(terminal.borrow().as_ref().unwrap().state,maho_codemode::tool::detached_cell_contract::EvalDetachedCellState::Cancelled);
    assert_eq!(recovered.unwrap().details["cells"][0]["output"],"42");
}
