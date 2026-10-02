use std::sync::{Arc, Mutex};
use serde_json::{Value, json};
use maho_ext_api::{AgentToolResult, ExecuteToolFuture, ExecuteToolOptions};
use maho_codemode::{bridges::{output_bridge::OutputExecuteTool, schema_bridge::EvalSchemaToolInfo}, config::settings::CodemodeSettings, extension::session_manager::{CodemodeSessionManager, CreateCodemodeSessionManagerOptions}, kernels::py::kernel_contract::PythonKernelRunOptions};

struct Fixture;
impl OutputExecuteTool for Fixture {
    fn execute_tool<'a>(&'a self, name: &'a str, params: Value, options: ExecuteToolOptions) -> ExecuteToolFuture<'a> {
        Box::pin(async move {
            assert!(!options.signal.expect("request signal").aborted());
            assert_eq!(name, "echo");
            Ok(AgentToolResult::text(params["text"].as_str().expect("echo text")))
        })
    }
}

#[tokio::test]
async fn persistent_python_calls_native_host_over_owned_bridge() {
    let artifacts = tempfile::tempdir().unwrap();
    let session = CodemodeSessionManager::start(CreateCodemodeSessionManagerOptions {
        session_id:"session-test".into(), cwd:artifacts.path().into(), settings:CodemodeSettings::default(),
        availability: [(maho_codemode::tool::types::EvalLanguage::Py,true),(maho_codemode::tool::types::EvalLanguage::Js,true),(maho_codemode::tool::types::EvalLanguage::Rb,true),(maho_codemode::tool::types::EvalLanguage::Jl,false)].map(|(language,enabled)|(language,maho_codemode::interpreters::detect::LanguageAvailability {enabled,detected:if language==maho_codemode::tool::types::EvalLanguage::Py {maho_codemode::interpreters::detect::InterpreterDetection::Detected {path:"python3".into(),version:"3".into(),resolved_path:None}}else {maho_codemode::interpreters::detect::InterpreterDetection::Unavailable}})),
        local_roots:None, artifacts_dir:Some(artifacts.path().into()), session_env:None,
        executor:Arc::new(Fixture),
        list_tools:Some(Arc::new(|| vec![EvalSchemaToolInfo { name:"echo".into(), description:None, parameters:Some(json!({"type":"object"})) }])),
        complete:Arc::new(|request|Box::pin(async move { Ok(json!({"text":request.prompt,"details":{"model":"fixture/model","structured":false}})) })),
    }).await.unwrap();
    let port = session.bridge_endpoint().unwrap().0;
    let kernel = session.get_python_kernel("python3").await.unwrap();
    assert!(Arc::ptr_eq(&kernel, &session.get_python_kernel("python3").await.unwrap()));
    let native=maho_codemode::tool::eval_tool_options::EvalKernelManager::get_kernel(&session,maho_codemode::tool::types::EvalLanguage::Py).await.unwrap();
    let native_again=maho_codemode::tool::eval_tool_options::EvalKernelManager::get_kernel(&session,maho_codemode::tool::types::EvalLanguage::Py).await.unwrap();
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
    session.dispose().await.unwrap();
    session.dispose().await.unwrap();
    assert!(session.get_python_kernel("python3").await.is_err());
    assert!(tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err());
}
