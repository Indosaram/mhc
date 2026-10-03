use std::{collections::HashMap,sync::{Arc,Mutex}};
use serde_json::json;
use maho_codemode::{tool::{eval_tool_options::*,types::*,image_resize::*,detached_cell_manager::*,run_eval_cell::run_eval_cell},kernels::py::{kernel::PythonKernel,kernel_contract::PythonKernelStartOptions},bridge::protocol::BridgeConnectionConfig};

struct Manager(Arc<PythonKernel>);
struct StartedPython(Arc<PythonKernel>,tokio::sync::mpsc::UnboundedSender<()>);
impl EvalKernel for StartedPython {
    fn run(&self,mut input:EvalKernelRunInput)->EvalKernelFuture<'_,serde_json::Value> {
        let original=input.on_started.take();let sender=self.1.clone();
        input.on_started=Some(Arc::new(move ||{if let Some(started)=&original {started();}let _=sender.send(());}));
        EvalKernel::run(self.0.as_ref(),input)
    }
    fn interrupt<'a>(&'a self,reason:&'a str,id:Option<&'a str>)->EvalKernelFuture<'a,KernelInterruptHandle> {EvalKernel::interrupt(self.0.as_ref(),reason,id)}
    fn cancel_queued<'a>(&'a self,id:&'a str,reason:&'a str)->EvalKernelFuture<'a,bool> {EvalKernel::cancel_queued(self.0.as_ref(),id,reason)}
    fn queue_snapshot(&self)->(Option<String>,Vec<String>) {self.0.queue_snapshot()}
    fn deliver_tool_reply(&self,message:serde_json::Value)->Result<(),String> {EvalKernel::deliver_tool_reply(self.0.as_ref(),message)}
    fn reset(&self)->EvalKernelFuture<'_,()> {EvalKernel::reset(self.0.as_ref())}
    fn close(&self)->EvalKernelFuture<'_,()> {EvalKernel::close(self.0.as_ref())}
}
struct StartedPythonManager(Arc<StartedPython>);
impl EvalKernelManager for StartedPythonManager {
    fn get_kernel(&self,_:EvalLanguage)->EvalKernelFuture<'_,Arc<dyn EvalKernel>> {Box::pin(async {Ok(self.0.clone() as Arc<dyn EvalKernel>)})}
}
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
struct TranscriptExecutor(Mutex<Vec<(String,serde_json::Value)>>);
struct SchemaExecutor(std::sync::atomic::AtomicUsize);
impl maho_codemode::bridges::output_bridge::OutputExecuteTool for SchemaExecutor {
    fn execute_tool<'a>(&'a self,name:&'a str,_:serde_json::Value,_:maho_ext_api::ExecuteToolOptions)->maho_ext_api::ExecuteToolFuture<'a> {
        self.0.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move {Err(maho_ext_api::ExecuteToolError {code:maho_ext_api::ExecuteToolErrorCode::InvalidParams,tool_name:name.into(),message:"invalid params".into(),active_tools:vec![]})})
    }
}

#[tokio::test]
async fn real_js_callable_schema_lookup_and_failure_enrichment() {
    let kernel=Arc::new(maho_codemode::kernels::js::context_manager::JavaScriptKernel::start(std::path::Path::new(env!("CARGO_MANIFEST_DIR")),"schema-callable",4,None).await.unwrap());
    let executor=Arc::new(SchemaExecutor(std::sync::atomic::AtomicUsize::new(0)));
    let schema=json!({"type":"object","required":["path"],"properties":{"path":{"type":"string"}}});
    let catalog_schema=schema.clone();
    let options=Arc::new(CreateEvalToolOptions {kernel_manager:Arc::new(JsManager(kernel.clone())),executor:executor.clone(),list_tools:Some(Arc::new(move ||Ok(vec![maho_codemode::bridges::schema_bridge::EvalSchemaToolInfo {name:"fixture_read".into(),description:None,parameters:Some(catalog_schema.clone())}]))),complete:None,settings:Default::default(),artifacts_dir:None,image_sdk:Arc::new(Images),cell_manager:Arc::new(Mutex::new(EvalDetachedCellManager::new(Default::default()))),on_cell_settled:None,prompt:Default::default(),runtimes:HashMap::new(),mode:"print".into()});
    let tool=maho_codemode::tool::eval_tool::create_eval_tool(options).unwrap();
    let lookup=(tool.execute)(maho_tools::definition::ToolCall {id:"schema-lookup",params:json!({"language":"js","code":"var expected = await tool_schema('fixture_read'); expected.parameters.required[0]","summary":"schema lookup"}),signal:Default::default(),on_update:None,context:None}).await;
    let lookup_calls=executor.0.load(std::sync::atomic::Ordering::SeqCst);
    let enrichment=(tool.execute)(maho_tools::definition::ToolCall {id:"schema-enrichment",params:json!({"language":"js","code":"try { await tool.fixture_read({}); } catch (error) { console.log(error.message.includes('Expected parameters:') && error.message.includes('path: string')); }","summary":"schema failure"}),signal:Default::default(),on_update:None,context:None}).await;
    kernel.close().await.unwrap();
    assert!(kernel.pid().is_none());eprintln!("cleanup: schema callable worker closed; pid None");
    assert_eq!(lookup_calls,0,"tool_schema cannot invoke the host executor");
    let lookup=lookup.unwrap();
    assert!(lookup.content.iter().any(|part|matches!(part,maho_tools::definition::ToolContent::Text {text,..} if text.contains("path"))));
    let enrichment=enrichment.unwrap();
    assert!(enrichment.content.iter().any(|part|matches!(part,maho_tools::definition::ToolContent::Text {text,..} if text.contains("true"))));
    assert_eq!(executor.0.load(std::sync::atomic::Ordering::SeqCst),1);
}

impl maho_codemode::bridges::output_bridge::OutputExecuteTool for TranscriptExecutor {
    fn is_tool_available(&self,name:&str)->Option<bool> {Some(name=="named_output")}
    fn execute_tool<'a>(&'a self,name:&'a str,args:serde_json::Value,_:maho_ext_api::ExecuteToolOptions)->maho_ext_api::ExecuteToolFuture<'a> {
        Box::pin(async move {
            self.0.lock().expect("transcript calls").push((name.into(),args.clone()));
            let target=args.get("task_id").or_else(||args.get("name")).and_then(serde_json::Value::as_str).expect("output target");
            Ok(maho_ext_api::AgentToolResult::text(format!("TRANSCRIPT:{target}:{}\nsecond\nthird",args["mode"].as_str().expect("transcript mode"))))
        })
    }
}

#[tokio::test]
async fn real_js_callable_output_returns_configured_transcript() {
    let kernel=Arc::new(maho_codemode::kernels::js::context_manager::JavaScriptKernel::start(std::path::Path::new(env!("CARGO_MANIFEST_DIR")),"output-callable",4,None).await.unwrap());
    let executor=Arc::new(TranscriptExecutor(Mutex::new(vec![])));
    let mut settings=maho_codemode::config::settings::CodemodeSettings::default();settings.task_tools.output="named_output".into();
    let options=Arc::new(CreateEvalToolOptions {kernel_manager:Arc::new(JsManager(kernel.clone())),executor:executor.clone(),list_tools:None,complete:None,settings,artifacts_dir:None,image_sdk:Arc::new(Images),cell_manager:Arc::new(Mutex::new(EvalDetachedCellManager::new(Default::default()))),on_cell_settled:None,prompt:Default::default(),runtimes:HashMap::new(),mode:"print".into()});
    let tool=maho_codemode::tool::eval_tool::create_eval_tool(options).unwrap();
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),(tool.execute)(maho_tools::definition::ToolCall {id:"output-cell",params:json!({"language":"js","code":"await output('st_123')","summary":"transcript proof","on_timeout":"error"}),signal:Default::default(),on_update:None,context:None})).await;
    kernel.close().await.unwrap();
    let result=result.unwrap().unwrap();
    assert_ne!(result.details.as_ref().unwrap()["isError"],true,"{result:?}");
    assert!(result.details.as_ref().unwrap()["cells"][0]["output"].as_str().unwrap().contains("TRANSCRIPT:st_123:full"));
    assert_eq!(*executor.0.lock().unwrap(),vec![("named_output".into(),json!({"task_id":"st_123","mode":"full"}))]);
    assert_eq!(kernel.pid(),None);
}
async fn fixture()->(Arc<PythonKernel>,Arc<CreateEvalToolOptions>) {
    fixture_with_messages(None).await
}
async fn fixture_with_messages(on_message:Option<maho_codemode::kernels::shared::subprocess_run::KernelMessageCallback>)->(Arc<PythonKernel>,Arc<CreateEvalToolOptions>) {
    let kernel=Arc::new(PythonKernel::start(PythonKernelStartOptions {interpreter_path:"python3".into(),session_id:"eval-chain".into(),cwd:env!("CARGO_MANIFEST_DIR").into(),connection:BridgeConnectionConfig {port:1,token:"test".into(),local_roots:None,artifacts_dir:None,parallel_pool_width:None},env:None,session_env:None,startup_timeout:None,on_message}).await.expect("Python fixture startup"));
    let options=Arc::new(CreateEvalToolOptions {kernel_manager:Arc::new(Manager(kernel.clone())),executor:Arc::new(Executor),list_tools:None,complete:None,settings:Default::default(),artifacts_dir:None,image_sdk:Arc::new(Images),cell_manager:Arc::new(Mutex::new(EvalDetachedCellManager::new(Default::default()))),on_cell_settled:None,prompt:Default::default(),runtimes:HashMap::new(),mode:"print".into()});
    (kernel,options)
}
fn invocation(id:&str,code:&str)->EvalCellInvocation {
    EvalCellInvocation {steering_signal:None,cell_id:id.into(),input:EvalToolInput {language:EvalLanguage::Py,code:code.into(),summary:"compute a value".into(),action:None,timeout:None,on_timeout:Some(TimeoutBehavior::Error),reset:None},signal:maho_ai::utils::abort::AbortController::new().signal(),on_update:None,mode:"print".into(),model:None,context:None}
}

#[tokio::test]
async fn real_python_deadline_reports_preserved_state_and_cleans_worker() {
    let (ready,mut started)=tokio::sync::mpsc::unbounded_channel();
    let (kernel,mut options)=fixture().await;
    let initialized=run_eval_cell(options.clone(),invocation("timeout-init","retained_timeout_value = 41")).await;
    Arc::get_mut(&mut options).unwrap().kernel_manager=Arc::new(StartedPythonManager(Arc::new(StartedPython(kernel.clone(),ready))));
    let mut call=invocation("timeout-state","print('TIMEOUT_READY',flush=True)\nwhile True: pass");
    let caller=maho_ai::utils::abort::AbortController::new();
    call.signal=caller.signal();
    let trigger=async {let ready=tokio::time::timeout(std::time::Duration::from_secs(5),started.recv()).await;caller.abort(Some(maho_ai::utils::abort::AbortReason::new("TimeoutError","test deadline")));ready};
    let (timed,ready)=tokio::join!(tokio::time::timeout(std::time::Duration::from_secs(8),run_eval_cell(options.clone(),call)),trigger);
    let retained=run_eval_cell(options.clone(),invocation("timeout-after","retained_timeout_value + 1")).await;
    kernel.close().await.unwrap();
    eprintln!("cleanup: timeout-state Python worker closed");
    ready.unwrap().unwrap();
    initialized.unwrap();
    let timed=timed.unwrap().unwrap();
    assert_eq!(timed.details["isError"],true);
    let retained=retained.unwrap();
    assert_eq!(retained.details["isError"],serde_json::Value::Null);
    assert!(retained.content.iter().any(|part|matches!(part,maho_ext_api::ContentBlock::Text(text) if text.text.trim()=="42")));
    let snapshot=options.cell_manager.lock().unwrap().peek("timeout-state").unwrap();
    assert_eq!(snapshot.state_retained,Some(true));
}

struct FinalFrameKernel;

#[tokio::test]
async fn interactive_steering_detaches_real_started_cell() {
    let (kernel,mut options)=fixture().await;
    Arc::get_mut(&mut options).unwrap().settings.cell_timeout_seconds=30.0;
    let (started,mut events)=tokio::sync::mpsc::unbounded_channel();
    Arc::get_mut(&mut options).unwrap().kernel_manager=Arc::new(StartedPythonManager(Arc::new(StartedPython(kernel.clone(),started))));
    let steering=maho_ext_api::AbortSignal::default();
    let mut call=invocation("steering","while True: pass");call.mode="interactive".into();call.input.on_timeout=Some(TimeoutBehavior::Detach);call.steering_signal=Some(steering.clone());
    let trigger=async {events.recv().await.unwrap();steering.abort();};
    let (result,())=tokio::join!(run_eval_cell(options.clone(),call),trigger);
    let stopped=EvalDetachedCellManager::stop(&options.cell_manager,"steering","test cleanup").await;
    kernel.close().await.unwrap();
    assert_eq!(result.unwrap().details["cells"][0]["status"],"detached");
    assert!(stopped.is_ok());
}

#[tokio::test]
async fn real_js_deadline_reports_restarted_state_and_recovery() {
    let kernel=Arc::new(maho_codemode::kernels::js::context_manager::JavaScriptKernel::start(std::path::Path::new(env!("CARGO_MANIFEST_DIR")),"timeout-js",4,None).await.unwrap());
    let options=Arc::new(CreateEvalToolOptions {kernel_manager:Arc::new(JsManager(kernel.clone())),executor:Arc::new(Executor),list_tools:None,complete:None,settings:Default::default(),artifacts_dir:None,image_sdk:Arc::new(Images),cell_manager:Arc::new(Mutex::new(EvalDetachedCellManager::new(Default::default()))),on_cell_settled:None,prompt:Default::default(),runtimes:HashMap::new(),mode:"print".into()});
    let mut call=invocation("timeout-js-state","globalThis.timeoutMarker=41; console.log('TIMEOUT_READY'); while(true) {}");
    call.input.language=EvalLanguage::Js;
    let caller=maho_ai::utils::abort::AbortController::new();call.signal=caller.signal();
    let (ready,mut started)=tokio::sync::mpsc::unbounded_channel();
    call.on_update=Some(Arc::new(move |result|{if result.content.iter().any(|part|matches!(part,maho_ext_api::ContentBlock::Text(text) if text.text.contains("TIMEOUT_READY"))) {let _=ready.send(());}}));
    let trigger=async {tokio::time::timeout(std::time::Duration::from_secs(5),started.recv()).await.unwrap().unwrap();caller.abort(Some(maho_ai::utils::abort::AbortReason::new("TimeoutError","test deadline")));};
    let (timed,())=tokio::join!(tokio::time::timeout(std::time::Duration::from_secs(8),run_eval_cell(options.clone(),call)),trigger);
    let mut next=invocation("timeout-js-after","typeof timeoutMarker");next.input.language=EvalLanguage::Js;
    let recovered=run_eval_cell(options.clone(),next).await;
    kernel.close().await.unwrap();assert!(kernel.pid().is_none());
    eprintln!("cleanup: timeout-state JS worker closed; pid None");
    let timed=timed.unwrap().unwrap();
    assert_eq!(timed.details["isError"],true);
    let recovered=recovered.unwrap();
    assert!(recovered.content.iter().any(|part|matches!(part,maho_ext_api::ContentBlock::Text(text) if text.text.contains("undefined"))));
    assert_eq!(options.cell_manager.lock().unwrap().peek("timeout-js-state").unwrap().state_retained,Some(false));
}
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
struct FailedInterruptKernel(tokio::sync::mpsc::UnboundedSender<()>);
impl EvalKernel for FailedInterruptKernel {
    fn run(&self,input:EvalKernelRunInput)->EvalKernelFuture<'_,serde_json::Value> {Box::pin(async move {input.on_started.ok_or("missing start callback")?();self.0.send(()).map_err(|error|error.to_string())?;std::future::pending().await})}
    fn interrupt<'a>(&'a self,_:&'a str,_:Option<&'a str>)->EvalKernelFuture<'a,KernelInterruptHandle> {Box::pin(async {Err("interrupt transport failed".into())})}
    fn cancel_queued<'a>(&'a self,id:&'a str,reason:&'a str)->EvalKernelFuture<'a,bool> {FinalFrameKernel.cancel_queued(id,reason)}
    fn queue_snapshot(&self)->(Option<String>,Vec<String>) {(None,vec![])}
    fn deliver_tool_reply(&self,message:serde_json::Value)->Result<(),String> {FinalFrameKernel.deliver_tool_reply(message)}
    fn reset(&self)->EvalKernelFuture<'_,()> {FinalFrameKernel.reset()}
    fn close(&self)->EvalKernelFuture<'_,()> {FinalFrameKernel.close()}
}
struct FailedInterruptManager(Arc<FailedInterruptKernel>);
impl EvalKernelManager for FailedInterruptManager {
    fn get_kernel(&self,_:EvalLanguage)->EvalKernelFuture<'_,Arc<dyn EvalKernel>> {Box::pin(async {Ok(self.0.clone() as Arc<dyn EvalKernel>)})}
}

#[tokio::test(start_paused=true)]
async fn timeout_interrupt_error_does_not_claim_retained_state() {
    let (started,mut events)=tokio::sync::mpsc::unbounded_channel();
    let options=Arc::new(CreateEvalToolOptions {kernel_manager:Arc::new(FailedInterruptManager(Arc::new(FailedInterruptKernel(started)))),executor:Arc::new(Executor),list_tools:None,complete:None,settings:Default::default(),artifacts_dir:None,image_sdk:Arc::new(Images),cell_manager:Arc::new(Mutex::new(EvalDetachedCellManager::new(Default::default()))),on_cell_settled:None,prompt:Default::default(),runtimes:HashMap::new(),mode:"print".into()});
    let caller=maho_ai::utils::abort::AbortController::new();
    let mut call=invocation("failed-interrupt","unused");call.signal=caller.signal();
    let trigger=async {events.recv().await.unwrap();caller.abort(Some(maho_ai::utils::abort::AbortReason::new("TimeoutError","test deadline")));};
    let (result,())=tokio::join!(run_eval_cell(options.clone(),call),trigger);
    assert_eq!(result.unwrap().details["isError"],true);
    assert_eq!(options.cell_manager.lock().unwrap().peek("failed-interrupt").unwrap().state_retained,None);
}
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
struct ProgressExecutor(tokio::sync::Notify);
impl maho_codemode::bridges::output_bridge::OutputExecuteTool for ProgressExecutor {
    fn execute_tool<'a>(&'a self,_:&'a str,_:serde_json::Value,options:maho_ext_api::ExecuteToolOptions)->maho_ext_api::ExecuteToolFuture<'a> {
        Box::pin(async move {
            if let Some(update)=options.on_update {
                let mut result=maho_ext_api::AgentToolResult::text("progress");
                result.details=json!({"task_id":"st_ab","status":"running","agent":"worker"});
                update(result);
            }
            self.0.notified().await;
            Ok(maho_ext_api::AgentToolResult::text("done"))
        })
    }
}

#[tokio::test]
async fn real_js_agent_progress_reaches_live_cell_before_task_settles() {
    let kernel=Arc::new(maho_codemode::kernels::js::context_manager::JavaScriptKernel::start(std::path::Path::new(env!("CARGO_MANIFEST_DIR")),"eval-progress",4,None).await.unwrap());
    let executor=Arc::new(ProgressExecutor(tokio::sync::Notify::new()));
    let options=Arc::new(CreateEvalToolOptions {kernel_manager:Arc::new(JsManager(kernel.clone())),executor:executor.clone(),list_tools:None,complete:None,settings:Default::default(),artifacts_dir:None,image_sdk:Arc::new(Images),cell_manager:Arc::new(Mutex::new(EvalDetachedCellManager::new(Default::default()))),on_cell_settled:None,prompt:Default::default(),runtimes:HashMap::new(),mode:"print".into()});
    let (updates,mut events)=tokio::sync::mpsc::unbounded_channel();
    let mut input=invocation("agent-progress","await agent('work'); 42");input.input.language=EvalLanguage::Js;
    input.on_update=Some(Arc::new(move |result|{if result.details["statusEvents"][0]["id"]=="st_ab" {let _=updates.send(result);}}));
    let run=tokio::spawn(run_eval_cell(options,input));
    let progress=tokio::time::timeout(std::time::Duration::from_secs(2),events.recv()).await;
    executor.0.notify_one();
    let result=run.await;
    kernel.close().await.unwrap();
    let progress=progress.expect("agent progress must precede task completion").unwrap();
    assert_eq!(progress.details["statusEvents"][0]["agent"],"worker");
    assert_eq!(result.unwrap().unwrap().details["statusEvents"][0]["id"],"st_ab");
}
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
