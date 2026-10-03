use std::{collections::HashMap, sync::{Arc, Mutex}, time::Duration};
use maho_codemode::{extension::session_manager_proxy::{SessionManagerProxy, SessionManagerLifecycle, SessionDisposeFuture}, kernels::js::context_manager::JavaScriptKernel, tool::{eval_tool::create_eval_tool, eval_tool_options::*, types::*, detached_cell_manager::EvalDetachedCellManager, image_resize::*}};
use maho_tools::definition::{ToolCall, AbortSignal};
use serde_json::json;

struct Manager(Arc<JavaScriptKernel>);
impl EvalKernelManager for Manager {
    fn get_kernel(&self, _: EvalLanguage) -> EvalKernelFuture<'_, Arc<dyn EvalKernel>> {
        Box::pin(async { Ok(self.0.clone() as Arc<dyn EvalKernel>) })
    }
}
impl SessionManagerLifecycle for Manager {
    fn dispose(&self) -> SessionDisposeFuture<'_> {
        Box::pin(async { self.0.close().await.map_err(|error| error.to_string()) })
    }
}
struct Images;
impl EvalImageSdk for Images {
    fn resize_image<'a>(&'a self, _: Vec<u8>, _: &'a str, _: Option<usize>) -> ImageFuture<'a, Option<ResizedImage>> { Box::pin(async { panic!("text-only scenario") }) }
    fn convert_to_png<'a>(&'a self, _: &'a str, _: &'a str) -> ImageFuture<'a, Option<EvalImageContent>> { Box::pin(async { panic!("text-only scenario") }) }
}
struct Executor(tokio::sync::mpsc::UnboundedSender<()>);
impl maho_codemode::bridges::output_bridge::OutputExecuteTool for Executor {
    fn execute_tool<'a>(&'a self, _: &'a str, _: serde_json::Value, options: maho_ext_api::ExecuteToolOptions) -> maho_ext_api::ExecuteToolFuture<'a> {
        Box::pin(async move {
            self.0.send(()).expect("host call receiver");
            options.signal.expect("host call cancellation signal").cancelled().await;
            Err(maho_ext_api::ExecuteToolError { code: maho_ext_api::ExecuteToolErrorCode::Blocked, tool_name: "park".into(), message: "cancelled host call".into(), active_tools: vec![] })
        })
    }
}

struct CapacityKernel {
    admitted: tokio::sync::mpsc::UnboundedSender<String>,
    pending: Mutex<HashMap<String,tokio::sync::oneshot::Sender<serde_json::Value>>>,
}
impl EvalKernel for CapacityKernel {
    fn run(&self,input:EvalKernelRunInput)->EvalKernelFuture<'_,serde_json::Value> {
        Box::pin(async move {
            let (sender,receiver)=tokio::sync::oneshot::channel();
            self.pending.lock().map_err(|error|error.to_string())?.insert(input.cell_id.clone(),sender);
            if let Some(started)=input.on_started {started();}
            self.admitted.send(input.cell_id).map_err(|error|error.to_string())?;
            receiver.await.map_err(|error|error.to_string())
        })
    }
    fn cancel_queued<'a>(&'a self,_:&'a str,_:&'a str)->EvalKernelFuture<'a,bool> {Box::pin(async {Ok(false)})}
    fn interrupt<'a>(&'a self,_:&'a str,id:Option<&'a str>)->EvalKernelFuture<'a,KernelInterruptHandle> {
        Box::pin(async move {
            if let Some(id)=id && let Some(sender)=self.pending.lock().map_err(|error|error.to_string())?.remove(id) {let _=sender.send(json!({"type":"result","cellId":id,"ok":false,"error":{"message":"interrupted"},"durationMs":0}));}
            Ok(KernelInterruptHandle {state_retained:Box::pin(async {Ok(true)}),note:None})
        })
    }
    fn queue_snapshot(&self)->(Option<String>,Vec<String>) {(None,vec![])}
    fn deliver_tool_reply(&self,_:serde_json::Value)->Result<(),String> {panic!("capacity fixture has no host calls")}
    fn reset(&self)->EvalKernelFuture<'_,()> {Box::pin(async {Ok(())})}
    fn close(&self)->EvalKernelFuture<'_,()> {Box::pin(async {Ok(())})}
}
struct CapacityManager(Arc<CapacityKernel>);
impl EvalKernelManager for CapacityManager {
    fn get_kernel(&self,_:EvalLanguage)->EvalKernelFuture<'_,Arc<dyn EvalKernel>> {Box::pin(async {Ok(self.0.clone() as Arc<dyn EvalKernel>)})}
}

#[tokio::test(start_paused = true)]
async fn resolved_file_and_environment_capacity_is_enforced_by_callable() {
    use maho_codemode::{config::settings::*,tool::detached_cell_manager::DetachedCellManagerOptions};
    for (file,environment,capacity) in [(json!({}),None,15),(json!({"maxDetachedCells":4}),None,4),(json!({"maxDetachedCells":4}),Some("2"),2),(json!({"maxDetachedCells":4}),Some("0"),4),(json!({"maxDetachedCells":4}),Some("bad"),4)] {
        let root=tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".maho")).unwrap();
        std::fs::write(root.path().join(".maho/codemode.json"),file.to_string()).unwrap();
        let mut loaded=load_codemode_settings(root.path(),root.path()).await.unwrap();
        assert!(loaded.warnings.is_empty());
        let environment=environment.map(|value|Environment::from([("SENPI_CODEMODE_MAX_DETACHED_CELLS".into(),value.into())])).unwrap_or_default();
        loaded.settings.max_detached_cells=resolve_max_detached_cells(&loaded.settings,&environment);
        loaded.settings.foreground_window_seconds=1.0;
        let cells=Arc::new(Mutex::new(EvalDetachedCellManager::new(DetachedCellManagerOptions {max_detached_cells:loaded.settings.max_detached_cells.ceil() as usize,..Default::default()})));
        let (sender,mut admitted)=tokio::sync::mpsc::unbounded_channel();
        let kernel=Arc::new(CapacityKernel {admitted:sender,pending:Mutex::new(HashMap::new())});
        let (host,_unused)=tokio::sync::mpsc::unbounded_channel();
        let tool=create_eval_tool(Arc::new(CreateEvalToolOptions {kernel_manager:Arc::new(CapacityManager(kernel)),executor:Arc::new(Executor(host)),list_tools:None,complete:None,settings:loaded.settings,artifacts_dir:None,image_sdk:Arc::new(Images),cell_manager:cells.clone(),on_cell_settled:None,prompt:Default::default(),runtimes:HashMap::new(),mode:"tui".into()})).unwrap();
        for index in 0..=capacity {
            let execute=tool.execute.clone();
            let call=tokio::spawn(async move {execute(ToolCall {id:&format!("capacity-{index}"),params:json!({"language":"js","code":"fixture","summary":"capacity"}),signal:AbortSignal::default(),on_update:None,context:None}).await});
            assert_eq!(admitted.recv().await.unwrap(),format!("capacity-{index}"));
            tokio::time::advance(Duration::from_secs(1)).await;
            let result=call.await.unwrap().unwrap();
            if index<capacity {assert_eq!(result.details.as_ref().unwrap()["cells"][0]["status"],"detached");} else {assert_eq!(result.details.as_ref().unwrap()["isError"],true);}
        }
        assert_eq!(cells.lock().unwrap().list().0.len(),capacity);
        EvalDetachedCellManager::dispose(&cells).await.unwrap();
        assert!(cells.lock().unwrap().list().0.is_empty());
    }
}

#[tokio::test]
async fn callable_eval_is_cancelled_by_session_replacement_before_worker_retirement() {
    let kernel=Arc::new(JavaScriptKernel::start(std::path::Path::new(env!("CARGO_MANIFEST_DIR")), "callable-lifecycle", 4, None).await.unwrap());
    let proxy=Arc::new(SessionManagerProxy::default());
    assert!(proxy.replace(proxy.begin_replacement(), Arc::new(Manager(kernel.clone()))).await);
    let (started, mut events)=tokio::sync::mpsc::unbounded_channel();
    let options=Arc::new(CreateEvalToolOptions { kernel_manager:proxy.clone(), executor:Arc::new(Executor(started)), list_tools:None, complete:None, settings:Default::default(), artifacts_dir:None, image_sdk:Arc::new(Images), cell_manager:Arc::new(Mutex::new(EvalDetachedCellManager::new(Default::default()))), on_cell_settled:None, prompt:Default::default(), runtimes:HashMap::new(), mode:"print".into() });
    let tool=create_eval_tool(options).unwrap();
    let execute=tool.execute.clone();
    let mut run=tokio::spawn(async move { execute(ToolCall { id:"lifecycle-cell", params:json!({"language":"js","code":"await tool.park({}); 42", "summary":"lifecycle proof", "on_timeout":"error"}), signal:AbortSignal::default(), on_update:None, context:None }).await });
    let startup=tokio::time::timeout(Duration::from_secs(10), events.recv()).await;
    let result=if matches!(&startup, Ok(Some(()))) {
        proxy.begin_replacement();
        Some(tokio::time::timeout(Duration::from_secs(2), &mut run).await)
    } else {None};
    if !matches!(&result, Some(Ok(_))) {
        run.abort();
        let _=run.await;
    }
    proxy.dispose().await;
    let closed=kernel.close().await;
    startup.expect("host callback must arrive before replacement").expect("host callback channel closed");
    closed.unwrap();
    assert!(kernel.pid().is_none());
    eprintln!("cleanup: callable-lifecycle worker closed; pid None");
    let result=result.unwrap().expect("replacement must cancel the actual callable before disposal").unwrap();
    let result=result.unwrap();
    assert_eq!(result.details.as_ref().unwrap()["cells"][0]["status"], "error", "{result:?}");
    let listed=(tool.execute)(ToolCall {id:"list-after-dispose", params:json!({"action":"list"}), signal:AbortSignal::default(), on_update:None, context:None}).await.unwrap();
    assert!(listed.details.is_some());
    let rejected=(tool.execute)(ToolCall {id:"run-after-dispose", params:json!({"language":"js","code":"42","summary":"reject stale session"}), signal:AbortSignal::default(), on_update:None, context:None}).await.unwrap_err();
    assert!(rejected.to_string().contains("disposed"));
}
