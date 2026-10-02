use maho_codemode::kernels::js::context_manager::JavaScriptKernel;
use maho_codemode::kernels::shared::subprocess_contract::KernelRunInput;
use std::path::Path;

#[tokio::test]
async fn real_bun_persistent_kernel() {
    let kernel = JavaScriptKernel::start(Path::new(env!("CARGO_MANIFEST_DIR")), "bun-test", 4, None).await.unwrap();
    for (code, expected) in [("1+1", "2"), ("var x=7; x", "7"), ("x+1", "8")] {
        let result = kernel.run(KernelRunInput { cell_id: "c".into(), code: code.into(), timeout_ms: Some(5_000) }, |_| {}).await.unwrap();
        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["valueRepr"], expected);
    }
    kernel.close().await.unwrap();
}

#[tokio::test]
async fn real_bun_timeout_kills_worker_and_recovers() {
    let kernel = JavaScriptKernel::start(Path::new(env!("CARGO_MANIFEST_DIR")), "bun-timeout", 4, None).await.unwrap();
    let old = kernel.pid().unwrap();
    let result = kernel.run(KernelRunInput { cell_id: "runaway".into(), code: "while(true) {}".into(), timeout_ms: Some(20) }, |_| {}).await.unwrap();
    assert_eq!(result["ok"], false);
    assert!(!Path::new(&format!("/proc/{old}")).exists());
    let result = kernel.run(KernelRunInput { cell_id: "after".into(), code: "1+1".into(), timeout_ms: Some(5_000) }, |_| {}).await.unwrap();
    assert_eq!(result["valueRepr"], "2");
    kernel.close().await.unwrap();
}

#[tokio::test]
async fn callback_run_admission_preserves_worker_output_and_queue_snapshot() {
    use std::sync::{Arc,Mutex};
    let kernel=JavaScriptKernel::start(Path::new(env!("CARGO_MANIFEST_DIR")),"bun-callback",4,None).await.unwrap();
    let output=Arc::new(Mutex::new(String::new()));
    let observed=output.clone();
    let (sender,mut started)=tokio::sync::mpsc::unbounded_channel();
    let result=kernel.run_with_callbacks(KernelRunInput {cell_id:"callback".into(),code:"print('visible'); 42".into(),timeout_ms:Some(5000)},Some(Arc::new(move |message| {if let Some(text)=message["data"].as_str() {observed.lock().expect("output lock").push_str(text);}})),Some(Arc::new(move || {sender.send(()).expect("start receiver");}))).await.unwrap();
    started.recv().await.unwrap();
    assert_eq!(result["valueRepr"],"42");
    assert!(output.lock().unwrap().contains("visible"));
    assert!(!kernel.cancel_queued("missing","cancel").await);
    assert_eq!(kernel.queue_snapshot(),(None,vec![]));
    kernel.close().await.unwrap();
}

#[tokio::test]
async fn pulled_host_call_reply_resumes_real_worker() {
    let kernel=JavaScriptKernel::start(Path::new(env!("CARGO_MANIFEST_DIR")),"bun-pull",4,None).await.unwrap();
    let run=kernel.run_with_callbacks(KernelRunInput {cell_id:"pull".into(),code:"await tool.echo({value:42})".into(),timeout_ms:Some(5000)},None,None);
    let reply=async {
        let call=tokio::time::timeout(std::time::Duration::from_secs(3),kernel.next_tool_call()).await.unwrap().unwrap();
        assert_eq!(call["toolName"],"echo");
        assert_eq!(call["args"]["value"],42);
        kernel.deliver_tool_reply(serde_json::json!({"type":"tool-reply","callId":call["callId"],"ok":true,"value":42})).unwrap();
    };
    let (result,())=tokio::join!(run,reply);
    assert_eq!(result.unwrap()["valueRepr"],"42");
    kernel.close().await.unwrap();
}

#[tokio::test]
async fn cooperative_interrupt_preserves_live_worker_globals() {
    let kernel=JavaScriptKernel::start(Path::new(env!("CARGO_MANIFEST_DIR")),"bun-retained",4,None).await.unwrap();
    let pid=kernel.pid().unwrap();
    let run=kernel.run_with_callbacks(KernelRunInput {cell_id:"parked".into(),code:"var retained=41; await tool.park({})".into(),timeout_ms:None},None,None);
    let stop=async {
        tokio::time::timeout(std::time::Duration::from_secs(3),kernel.next_tool_call()).await.unwrap().unwrap();
        kernel.interrupt("test stop",Some("parked")).await.unwrap()
    };
    let (result,retained)=tokio::join!(run,stop);
    let after=kernel.run(KernelRunInput {cell_id:"retained".into(),code:"retained+1".into(),timeout_ms:Some(5000)},|_|{}).await;
    let same_pid=kernel.pid()==Some(pid);
    kernel.close().await.unwrap();
    assert!(!result.unwrap()["ok"].as_bool().unwrap());
    assert!(retained,"bridge wait should settle cooperatively without losing globals");
    assert!(same_pid);
    assert_eq!(after.unwrap()["valueRepr"],"42");
}

#[tokio::test]
async fn live_kernel_tool_host_pump_describes_and_invokes_worker_definition() {
    use maho_codemode::kernels::js::kernel_tools_types::*;
    let kernel=JavaScriptKernel::start(Path::new(env!("CARGO_MANIFEST_DIR")),"bun-tools",4,None).await.unwrap();
    let defined=kernel.run(KernelRunInput {cell_id:"define".into(),code:"tool(function increment(value) { return value+1; })".into(),timeout_ms:Some(5000)},|_|{}).await.unwrap();
    let described=tokio::time::timeout(std::time::Duration::from_secs(5),kernel.describe_kernel_tools(&["increment".into()])).await.unwrap();
    let descriptor=described.as_ref().unwrap()["results"][0]["descriptor"].clone();
    let result=tokio::time::timeout(std::time::Duration::from_secs(5),kernel.invoke_kernel_tool(KernelToolsInvokeRequest {name:"increment".into(),kernel_generation:descriptor["kernel_generation"].as_u64().unwrap(),definition_revision:descriptor["definition_revision"].as_u64().unwrap(),args:serde_json::json!({"value":41}),call_id:"native-invoke".into()},KernelToolsInvokeOptions {signal:None,scope:None})).await.unwrap();
    kernel.close().await.unwrap();
    assert_eq!(defined["ok"],true);
    assert_eq!(result.unwrap(),42);
}

#[tokio::test]
async fn bridge_wait_timeout_retains_worker_state() {
    let kernel=JavaScriptKernel::start(Path::new(env!("CARGO_MANIFEST_DIR")),"bun-timeout-retained",4,None).await.unwrap();
    let pid=kernel.pid().unwrap();
    let run=kernel.run_with_callbacks(KernelRunInput {cell_id:"budget".into(),code:"globalThis.timeoutMarker=41; await tool.started({})".into(),timeout_ms:Some(400)},None,None);
    let observe=async {tokio::time::timeout(std::time::Duration::from_secs(3),kernel.next_tool_call()).await.unwrap().unwrap()};
    let (result,call)=tokio::join!(run,observe);
    let next=kernel.run(KernelRunInput {cell_id:"after-budget".into(),code:"timeoutMarker+1".into(),timeout_ms:Some(5000)},|_|{}).await;
    let same_pid=kernel.pid()==Some(pid);
    kernel.close().await.unwrap();
    assert_eq!(call["toolName"],"started");
    let result=result.unwrap();
    assert_eq!(result["ok"],false);
    assert_eq!(result["durationMs"],400);
    assert!(result["error"]["message"].as_str().unwrap().contains("timed out"));
    assert!(same_pid);
    assert_eq!(next.unwrap()["valueRepr"],"42");
}

#[tokio::test]
async fn reset_fences_old_kernel_tool_generation() {
    use maho_codemode::kernels::js::kernel_tools_types::*;
    let kernel=JavaScriptKernel::start(Path::new(env!("CARGO_MANIFEST_DIR")),"bun-generation",4,None).await.unwrap();
    let input=||KernelRunInput {cell_id:"define-generation".into(),code:"tool(function generation_probe() { return 42; })".into(),timeout_ms:Some(5000)};
    let first=kernel.run(input(),|_|{}).await.unwrap();
    let old=kernel.describe_kernel_tools(&["generation_probe".into()]).await.unwrap();
    let old=&old["results"][0]["descriptor"];
    let request=KernelToolsInvokeRequest {name:"generation_probe".into(),kernel_generation:old["kernel_generation"].as_u64().unwrap(),definition_revision:old["definition_revision"].as_u64().unwrap(),args:serde_json::json!({}),call_id:"old-generation".into()};
    kernel.reset().await.unwrap();
    let second=kernel.run(input(),|_|{}).await.unwrap();
    let stale=kernel.invoke_kernel_tool(request,KernelToolsInvokeOptions {signal:None,scope:None}).await;
    let fresh=kernel.describe_kernel_tools(&["generation_probe".into()]).await.unwrap();
    kernel.close().await.unwrap();
    assert_eq!(first["ok"],true);
    assert_eq!(second["ok"],true);
    assert_eq!(stale.unwrap_err().code,KernelToolErrorCode::KernelToolStale);
    assert!(fresh["results"][0]["descriptor"]["kernel_generation"].as_u64().unwrap()>old["kernel_generation"].as_u64().unwrap());
}

#[tokio::test]
async fn live_name_sources_refresh_before_every_cell_and_recovery() {
    use std::sync::{Arc,Mutex};
    let names=Arc::new(Mutex::new((vec!["host_reserved".to_string()],vec!["foreign_reserved".to_string()])));
    let source=names.clone();
    let kernel=JavaScriptKernel::start_with_names(Path::new(env!("CARGO_MANIFEST_DIR")),"bun-names",4,None,maho_codemode::bridge::protocol::BridgeConnectionConfig {port:1,token:"fixture".into(),local_roots:None,artifacts_dir:None,parallel_pool_width:None},Arc::new(move ||Ok(source.lock().unwrap().clone()))).await.unwrap();
    let mut results=Vec::new();
    for name in ["host_reserved","foreign_reserved"] {
        results.push(kernel.run(KernelRunInput {cell_id:name.into(),code:format!("tool(function {name}() {{ return 1; }})"),timeout_ms:Some(5000)},|_|{}).await.unwrap());
    }
    *names.lock().unwrap()=(vec!["later_host".into()],vec![]);
    results.push(kernel.run(KernelRunInput {cell_id:"refresh".into(),code:"tool(function later_host() { return 1; })".into(),timeout_ms:Some(5000)},|_|{}).await.unwrap());
    kernel.reset().await.unwrap();
    results.push(kernel.run(KernelRunInput {cell_id:"reset".into(),code:"tool(function later_host() { return 1; })".into(),timeout_ms:Some(5000)},|_|{}).await.unwrap());
    kernel.close().await.unwrap();
    for result in results {assert_eq!(result["ok"],false,"{result}");assert!(result["error"]["message"].as_str().unwrap().contains("collides"));}
}

#[tokio::test]
async fn scoped_kernel_tool_denies_host_call_without_leaking_scope() {
    use maho_codemode::kernels::js::kernel_tools_types::*;
    let kernel=JavaScriptKernel::start(Path::new(env!("CARGO_MANIFEST_DIR")),"bun-scope",4,None).await.unwrap();
    let defined=kernel.run(KernelRunInput {cell_id:"define-scope".into(),code:"tool(async function scoped_writer() { return await tool.write({value:42}); })".into(),timeout_ms:Some(5000)},|_|{}).await.unwrap();
    let described=kernel.describe_kernel_tools(&["scoped_writer".into()]).await.unwrap();
    let descriptor=&described["results"][0]["descriptor"];
    let request=KernelToolsInvokeRequest {name:"scoped_writer".into(),kernel_generation:descriptor["kernel_generation"].as_u64().unwrap(),definition_revision:descriptor["definition_revision"].as_u64().unwrap(),args:serde_json::json!({}),call_id:"scope-denied".into()};
    let parked=kernel.run_with_callbacks(KernelRunInput {cell_id:"scope-parent".into(),code:"await tool.parent({})".into(),timeout_ms:Some(5000)},None,None);
    let nested=async {
        let parent=kernel.next_tool_call().await.unwrap();
        let denied=tokio::time::timeout(std::time::Duration::from_secs(3),kernel.invoke_kernel_tool(request.clone(),KernelToolsInvokeOptions {signal:None,scope:Some(KernelToolsInvokeScope {allow:None,deny:Some(vec!["write".into()])})})).await;
        let invoke=kernel.invoke_kernel_tool(KernelToolsInvokeRequest {call_id:"scope-free".into(),..request},KernelToolsInvokeOptions {signal:None,scope:None});
        let reply=async {
            let call=kernel.next_tool_call().await.unwrap();
            kernel.deliver_tool_reply(serde_json::json!({"type":"tool-reply","callId":call["callId"],"ok":true,"value":42})).unwrap();
            call
        };
        let allowed=tokio::time::timeout(std::time::Duration::from_secs(3),async {tokio::join!(invoke,reply)}).await;
        kernel.deliver_tool_reply(serde_json::json!({"type":"tool-reply","callId":parent["callId"],"ok":true,"value":null})).unwrap();
        (denied,allowed)
    };
    let (parent,(denied,allowed))=tokio::join!(parked,nested);
    kernel.close().await.unwrap();
    assert_eq!(parent.unwrap()["ok"],true);
    let (allowed,call)=allowed.unwrap();
    assert_eq!(defined["ok"],true);
    assert_eq!(denied.unwrap().unwrap_err().code,KernelToolErrorCode::KernelToolHostDenied);
    assert_eq!(call["toolName"],"write","denied invoke must not enqueue a host call");
    assert_eq!(allowed.unwrap(),42);
}

#[tokio::test]
async fn cooperative_interrupt_settles_nested_host_wait_as_stale() {
    use maho_codemode::kernels::js::kernel_tools_types::*;
    let kernel=JavaScriptKernel::start(Path::new(env!("CARGO_MANIFEST_DIR")),"bun-nested-stop",4,None).await.unwrap();
    let parent=kernel.run_with_callbacks(KernelRunInput {cell_id:"nested-parent".into(),code:"tool(async function nested_lookup() { return await tool.read({}); }); await tool.parent({})".into(),timeout_ms:Some(5000)},None,None);
    let child=async {
        kernel.next_tool_call().await.unwrap();
        let described=kernel.describe_kernel_tools(&["nested_lookup".into()]).await.unwrap();
        let descriptor=&described["results"][0]["descriptor"];
        let invoke=kernel.invoke_kernel_tool(KernelToolsInvokeRequest {name:"nested_lookup".into(),kernel_generation:descriptor["kernel_generation"].as_u64().unwrap(),definition_revision:descriptor["definition_revision"].as_u64().unwrap(),args:serde_json::json!({}),call_id:"nested-stop".into()},KernelToolsInvokeOptions {signal:None,scope:None});
        let stop=async {
            let call=kernel.next_tool_call().await.unwrap();
            assert_eq!(call["toolName"],"read");
            kernel.interrupt("nested-test",Some("nested-parent")).await
        };
        let (invoke,stop)=tokio::join!(async {tokio::time::timeout(std::time::Duration::from_secs(3),invoke).await},stop);
        (invoke,stop)
    };
    let (parent,(child,stop))=tokio::join!(parent,child);
    kernel.close().await.unwrap();
    assert_eq!(parent.unwrap()["ok"],false);
    assert!(stop.unwrap());
    assert_eq!(child.unwrap().unwrap_err().code,KernelToolErrorCode::KernelToolStale);
}

#[tokio::test]
async fn forced_worker_retirement_settles_nested_host_wait_as_stale() {
    use maho_codemode::kernels::js::kernel_tools_types::*;
    let kernel=JavaScriptKernel::start(Path::new(env!("CARGO_MANIFEST_DIR")),"bun-forced-nested-stop",4,None).await.unwrap();
    let old=kernel.pid();
    let parent=kernel.run_with_callbacks(KernelRunInput {cell_id:"blocked-parent".into(),code:"tool(async function blocked_lookup() { return await tool.read({}); }); await tool.parent({}); while(true) {}".into(),timeout_ms:None},None,None);
    let child=async {
        let parent_call=kernel.next_tool_call().await.unwrap();
        let described=kernel.describe_kernel_tools(&["blocked_lookup".into()]).await.unwrap();
        let descriptor=&described["results"][0]["descriptor"];
        let invoke=kernel.invoke_kernel_tool(KernelToolsInvokeRequest {name:"blocked_lookup".into(),kernel_generation:descriptor["kernel_generation"].as_u64().unwrap(),definition_revision:descriptor["definition_revision"].as_u64().unwrap(),args:serde_json::json!({}),call_id:"forced-nested-stop".into()},KernelToolsInvokeOptions {signal:None,scope:None});
        let stop=async {
            kernel.next_tool_call().await.unwrap();
            kernel.deliver_tool_reply(serde_json::json!({"type":"tool-reply","callId":parent_call["callId"],"ok":true,"value":null})).unwrap();
            kernel.interrupt("forced-stop",Some("blocked-parent")).await
        };
        tokio::join!(async {tokio::time::timeout(std::time::Duration::from_secs(4),invoke).await},stop)
    };
    let (parent,(child,stop))=tokio::join!(parent,child);
    let recovered=kernel.run(KernelRunInput {cell_id:"after-forced-stop".into(),code:"42".into(),timeout_ms:Some(5000)},|_|{}).await;
    let fresh=kernel.pid()!=old;
    kernel.close().await.unwrap();
    assert_eq!(parent.unwrap()["ok"],false);
    assert!(!stop.unwrap());
    assert_eq!(child.unwrap().unwrap_err().code,KernelToolErrorCode::KernelToolStale);
    assert!(fresh);
    assert_eq!(recovered.unwrap()["valueRepr"],"42");
}
