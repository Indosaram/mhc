use std::sync::{Arc, Mutex};
use serde_json::{Value, json};
use maho_ext_api::{AgentToolResult, ExecuteToolFuture, ExecuteToolOptions};
use maho_codemode::{bridges::{output_bridge::OutputExecuteTool, schema_bridge::EvalSchemaToolInfo}, config::settings::CodemodeSettings, extension::session_manager::{CodemodeSessionManager, CreateCodemodeSessionManagerOptions}, kernels::py::kernel_contract::PythonKernelRunOptions};

struct Fixture;
impl OutputExecuteTool for Fixture {
    fn execute_tool<'a>(&'a self, name: &'a str, params: Value, options: ExecuteToolOptions) -> ExecuteToolFuture<'a> {
        Box::pin(async move {
            assert!(!options.signal.expect("request signal").aborted());
            if name=="task_output" {return Ok(AgentToolResult::text("transcript"));}
            if name=="inactive" {return Err(maho_ext_api::ExecuteToolError {code:maho_ext_api::ExecuteToolErrorCode::InactiveTool,tool_name:name.into(),message:"inactive fixture".into(),active_tools:vec!["echo".into()]});}
            assert_eq!(name, "echo");
            Ok(AgentToolResult::text(params["text"].as_str().expect("echo text")))
        })
    }
}

#[tokio::test]
async fn persistent_python_calls_native_host_over_owned_bridge() {
    let artifacts = tempfile::tempdir().unwrap();
    let catalog_calls=Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let catalog_count=catalog_calls.clone();
    let catalog_available=Arc::new(std::sync::atomic::AtomicBool::new(true));
    let available=catalog_available.clone();
    let session = Arc::new(CodemodeSessionManager::start(CreateCodemodeSessionManagerOptions {
        session_id:"session-test".into(), cwd:artifacts.path().into(), settings:CodemodeSettings::default(),
        availability: [(maho_codemode::tool::types::EvalLanguage::Py,true),(maho_codemode::tool::types::EvalLanguage::Js,true),(maho_codemode::tool::types::EvalLanguage::Rb,true),(maho_codemode::tool::types::EvalLanguage::Jl,false)].map(|(language,enabled)|(language,maho_codemode::interpreters::detect::LanguageAvailability {enabled,detected:if language==maho_codemode::tool::types::EvalLanguage::Py {maho_codemode::interpreters::detect::InterpreterDetection::Detected {path:"python3".into(),version:"3".into(),resolved_path:None}}else {maho_codemode::interpreters::detect::InterpreterDetection::Unavailable}})),
        local_roots:None, artifacts_dir:Some(artifacts.path().into()), session_env:None,
        executor:Arc::new(Fixture),
        list_tools:Some(Arc::new(move || {catalog_count.fetch_add(1,std::sync::atomic::Ordering::SeqCst);if !available.load(std::sync::atomic::Ordering::SeqCst) {return Err("catalog unavailable".into());}Ok(vec![EvalSchemaToolInfo { name:"echo".into(), description:None, parameters:Some(json!({"type":"object"})) }])})),
        complete:Arc::new(|request|Box::pin(async move { Ok(json!({"text":request.prompt,"details":{"model":"fixture/model","structured":false}})) })),
    }).await.unwrap());
    let port = session.bridge_endpoint().unwrap().0;
    let kernel = session.get_python_kernel("python3").await.unwrap();
    assert!(Arc::ptr_eq(&kernel, &session.get_python_kernel("python3").await.unwrap()));
    let native=maho_codemode::tool::eval_tool_options::EvalKernelManager::get_kernel(session.as_ref(),maho_codemode::tool::types::EvalLanguage::Py).await.unwrap();
    let proxy=maho_codemode::extension::session_manager_proxy::SessionManagerProxy::default();
    assert!(proxy.replace(proxy.begin_replacement(),session.clone()).await);
    let native_again=maho_codemode::tool::eval_tool_options::EvalKernelManager::get_kernel(&proxy,maho_codemode::tool::types::EvalLanguage::Py).await.unwrap();
    assert!(Arc::ptr_eq(&native,&native_again));
    assert!(session.get_subprocess_kernel(maho_codemode::tool::types::EvalLanguage::Jl).await.is_err());
    let messages = Arc::new(Mutex::new(Vec::new()));
    let output = messages.clone();
    let result = kernel.run(PythonKernelRunOptions {
        cell_id:"host-call".into(), timeout_ms:Some(5000), on_started:None,
        on_message:Some(Arc::new(move |message| output.lock().expect("output messages").push(message.clone()))),
        code:"x = 41\nr = tool.echo({'text': 'hello'})\nprint(r['text'])\nprint(tool_schema('echo')['name'])\nwrite('local://receipt.txt', 'owned')".into(),
    }).await.unwrap();
    assert_eq!(result["ok"], true);
    let output = messages.lock().unwrap().iter().filter_map(|message|message["data"].as_str()).collect::<String>();
    assert!(output.contains("hello"));
    assert!(output.contains("echo"));
    assert_eq!(tokio::fs::read_to_string(artifacts.path().join("local/receipt.txt")).await.unwrap(), "owned");
    let output = Arc::new(Mutex::new(String::new()));
    let next_output = output.clone();
    let next = kernel.run(PythonKernelRunOptions { cell_id:"persistent".into(), code:"print(x + 1)".into(), timeout_ms:Some(5000), on_started:None, on_message:Some(Arc::new(move |message| if let Some(text)=message["data"].as_str() { next_output.lock().expect("next output").push_str(text); })) }).await.unwrap();
    assert_eq!(next["ok"], true);
    assert!(output.lock().unwrap().contains("42"));
    let codes=Arc::new(Mutex::new(String::new()));let observed=codes.clone();
    let coded=kernel.run(PythonKernelRunOptions {cell_id:"coded-failure".into(),code:"try:\n tool.inactive({})\nexcept Exception as error:\n print(getattr(error, 'code', 'missing'))".into(),timeout_ms:Some(5000),on_started:None,on_message:Some(Arc::new(move |message|if let Some(text)=message["data"].as_str() {observed.lock().unwrap().push_str(text);} ))}).await.unwrap();
    catalog_available.store(false,std::sync::atomic::Ordering::SeqCst);
    let failure=kernel.run(PythonKernelRunOptions {cell_id:"catalog-failure".into(),code:"tool_schema('echo')".into(),timeout_ms:Some(5000),on_started:None,on_message:None}).await.unwrap();
    assert_eq!(failure["ok"],false);
    assert!(failure["error"]["message"].as_str().unwrap().contains("catalog unavailable"));
    let helper=kernel.run(PythonKernelRunOptions {cell_id:"output-without-catalog".into(),code:"output('st_fixture')".into(),timeout_ms:Some(5000),on_started:None,on_message:None}).await.unwrap();
    assert_eq!(catalog_calls.load(std::sync::atomic::Ordering::SeqCst),2,"Python ordinary and output calls must not query the schema catalog");
    assert!(session.get_javascript_kernel().await.is_err(),"JS startup must propagate catalog failure");
    catalog_available.store(true,std::sync::atomic::Ordering::SeqCst);
    let js=session.get_javascript_kernel().await.unwrap();
    let js_again=session.get_javascript_kernel().await.unwrap();
    assert!(Arc::ptr_eq(&js,&js_again));
    let js_native=maho_codemode::tool::eval_tool_options::EvalKernelManager::get_kernel(&proxy,maho_codemode::tool::types::EvalLanguage::Js).await.unwrap();
    let js_native_again=maho_codemode::tool::eval_tool_options::EvalKernelManager::get_kernel(&proxy,maho_codemode::tool::types::EvalLanguage::Js).await.unwrap();
    assert!(Arc::ptr_eq(&js_native,&js_native_again));
    let js_result=js.run(maho_codemode::kernels::shared::subprocess_contract::KernelRunInput {cell_id:"js-local".into(),code:"var sessionValue=41; await write('local://js-receipt.txt','owned-js'); await read('local://js-receipt.txt')".into(),timeout_ms:Some(5000)},|_|{}).await.unwrap();
    let js_next=js.run(maho_codemode::kernels::shared::subprocess_contract::KernelRunInput {cell_id:"js-state".into(),code:"sessionValue+1".into(),timeout_ms:Some(5000)},|_|{}).await.unwrap();
    let collision=js.run(maho_codemode::kernels::shared::subprocess_contract::KernelRunInput {cell_id:"host-collision".into(),code:"tool(function echo() { return 1; })".into(),timeout_ms:Some(5000)},|_|{}).await.unwrap();
    proxy.dispose().await;
    session.dispose().await.unwrap();
    assert_eq!(coded["ok"],true);
    assert!(codes.lock().unwrap().contains("inactive_tool"),"bridge must preserve native execute error code");
    assert_eq!(js_result["ok"],true,"{js_result}");
    assert_eq!(js_next["valueRepr"],"42");
    assert_eq!(collision["ok"],false);
    assert!(collision["error"]["message"].as_str().unwrap().contains("collides"));
    assert_eq!(tokio::fs::read_to_string(artifacts.path().join("local/js-receipt.txt")).await.unwrap(),"owned-js");
    assert!(js.pid().is_none());
    assert!(session.get_javascript_kernel().await.is_err());
    assert_eq!(helper["ok"],true,"output helper must not consult an unavailable schema registry");
    assert!(session.get_python_kernel("python3").await.is_err());
    assert!(tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err());
}

#[tokio::test]
async fn cancelled_invocation_cleanup_preserves_readmitted_same_id_http_context() {
    use maho_codemode::tool::{eval_tool_options::{EvalKernelManager,EvalInvocationContext},detached_cell_manager::{EvalDetachedCellManager,DetachedCellManagerOptions},types::{EvalToolInput,EvalLanguage}};
    use tokio::io::{AsyncReadExt,AsyncWriteExt};
    let root=tempfile::tempdir().unwrap();
    let seen=Arc::new(Mutex::new(Vec::new()));let observed=seen.clone();
    let session=CodemodeSessionManager::start(CreateCodemodeSessionManagerOptions {session_id:"ownership".into(),cwd:root.path().into(),settings:Default::default(),availability:[EvalLanguage::Py,EvalLanguage::Js,EvalLanguage::Rb,EvalLanguage::Jl].map(|language|(language,maho_codemode::interpreters::detect::LanguageAvailability {enabled:false,detected:maho_codemode::interpreters::detect::InterpreterDetection::Unavailable})),local_roots:None,artifacts_dir:None,session_env:None,executor:Arc::new(Fixture),list_tools:None,complete:Arc::new(move |request| {observed.lock().unwrap().push(request.context.map(|ctx|ctx.cwd));Box::pin(async {Ok(json!({}))})})}).await.unwrap();
    let mut cells=EvalDetachedCellManager::new(DetachedCellManagerOptions::default());
    let input=||EvalToolInput {language:EvalLanguage::Py,code:String::new(),summary:"ownership".into(),action:None,timeout:None,on_timeout:None,reset:None};
    let old=cells.create("same-id".into(),input()).unwrap();
    let context=|cwd:&str|EvalInvocationContext {model:None,cwd:cwd.into(),thinking_level:None,goal_store_file:None};
    let clear_old=session.set_invocation_context("same-id",context("/A")).unwrap();
    assert!(cells.create("same-id".into(),input()).is_err());
    assert!(cells.cancel_without_interrupt(&old));
    let _new=cells.create("same-id".into(),input()).unwrap();
    let clear_new=session.set_invocation_context("same-id",context("/B")).unwrap();
    clear_old();
    let (port,token)=session.bridge_endpoint().unwrap();
    let body=r#"{"prompt":"probe","cellId":"same-id"}"#;
    let request=format!("POST /completion HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",body.len());
    let response=tokio::time::timeout(std::time::Duration::from_secs(3),async {
        let mut socket=tokio::net::TcpStream::connect(("127.0.0.1",port)).await.unwrap();
        socket.write_all(request.as_bytes()).await.unwrap();
        let mut response=Vec::new();socket.read_to_end(&mut response).await.unwrap();response
    }).await;
    clear_new();session.dispose().await.unwrap();
    eprintln!("cleanup: same-ID context probe bridge disposed; response={response:?}");
    assert!(response.is_ok());
    assert_eq!(*seen.lock().unwrap(),vec![Some(std::path::PathBuf::from("/B"))]);
}
