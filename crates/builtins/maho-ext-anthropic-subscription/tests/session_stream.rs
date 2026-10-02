use std::{collections::BTreeMap, os::unix::fs::PermissionsExt};
use maho_ext_anthropic_subscription::session_stream::{SessionRegistry, ResidentInput};
use serde_json::json;

#[tokio::test]
async fn registry_reuses_process_for_delta_and_resumes_on_prompt_drift() {
    let directory = tempfile::tempdir().expect("dir"); let executable = directory.path().join("claude");
    std::fs::write(&executable, "#!/usr/bin/python3\nimport json,sys,os\nfor line in sys.stdin:\n f=json.loads(line)\n if f['type']=='control_request':\n  print(json.dumps({'type':'control_response','response':{'subtype':'success','request_id':f['request_id'],'response':{}}}),flush=True)\n else:\n  print(json.dumps({'type':'user','isReplay':True,'uuid':f['uuid']}),flush=True)\n  print(json.dumps({'type':'assistant','uuid':'assistant-'+f['uuid'],'parent_tool_use_id':None}),flush=True)\n  print(json.dumps({'type':'result','subtype':'success','user_message_uuid':f['uuid'],'result':str(os.getpid())}),flush=True)\n").expect("script");
    std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o700)).expect("permissions");
    let environment = BTreeMap::new(); let custom = BTreeMap::new(); let mut registry = SessionRegistry::default();
    let mut processes = Vec::new();
    for (index, prompt) in ["original", "original", "changed"].into_iter().enumerate() {
        let messages: Vec<_> = (0..=index).map(|i| json!({"role":"user","content":format!("turn {i}")})).collect();
        let context = json!({"messages":messages}); let options = json!({"cwd":directory.path(),"systemPrompt":prompt});
        let mut delivered = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(10), registry.turn(ResidentInput { session:"session",account:"default",model:"model",context:&context,options:&options,executable:&executable,environment:&environment,auth_lane:"ambient",custom:&custom,tool_note:None,now:index as u64,transcript_available:true,signal:None }, |message| delivered.push(message))).await.expect("bounded turn").expect("resident turn");
        processes.push(delivered.last().expect("result")["result"].clone());
        assert_eq!(registry.bindings.get("session").expect("binding").sent_count,index+1);
        assert!(registry.entries["session"].assistant_uuid_by_index.contains_key(&(index+1)));
        let observation = &registry.last_decisions["session"];
        assert_eq!(observation.kind, ["bootstrap", "delta", "fork"][index]);
        assert_eq!(observation.delta_messages,1);
    }
    assert_eq!(processes[0],processes[1]); assert_ne!(processes[1],processes[2]);
    registry.close_all().await.expect("reaped"); assert!(registry.entries.is_empty());
}

#[tokio::test]
async fn interrupt_receipt_controls_keep_or_close_without_taint() {
    for keep in [true, false] {
        let directory = tempfile::tempdir().expect("dir"); let executable = directory.path().join("claude");
        let queued = if keep { "[]" } else { "['pending']" };
        std::fs::write(&executable, format!("#!/usr/bin/python3\nimport json,sys\nu=None\nfor line in sys.stdin:\n f=json.loads(line)\n if f['type']=='control_request':\n  r={{'still_queued':{queued}}} if f['request']['subtype']=='interrupt' else {{}}\n  print(json.dumps({{'type':'control_response','response':{{'subtype':'success','request_id':f['request_id'],'response':r}}}}),flush=True)\n  if f['request']['subtype']=='interrupt': print(json.dumps({{'type':'result','subtype':'success','user_message_uuid':u}}),flush=True)\n else:\n  u=f['uuid']\n  print(json.dumps({{'type':'user','isReplay':True,'uuid':u}}),flush=True)\n  print(json.dumps({{'type':'stream_event','event':{{'type':'message_start'}}}}),flush=True)\n")).expect("script");
        std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o700)).expect("permissions");
        let environment = BTreeMap::new(); let custom = BTreeMap::new(); let mut registry = SessionRegistry::default();
        let context = json!({"messages":[{"role":"user","content":"interrupt"}]}); let options = json!({"cwd":directory.path()});
        let controller = maho_ai::utils::abort::AbortController::new(); let signal = controller.signal();
        let result = tokio::time::timeout(std::time::Duration::from_secs(10),registry.turn(ResidentInput { session:"session",account:"default",model:"model",context:&context,options:&options,executable:&executable,environment:&environment,auth_lane:"ambient",custom:&custom,tool_note:None,now:1,transcript_available:true,signal:Some(&signal) }, |message| {
            if message["type"] == "stream_event" { controller.abort(None); }
            assert_ne!(message["type"], "result");
        })).await.expect("bounded interrupt").expect("interrupted turn");
        assert!(result.aborted);
        assert_eq!(registry.entries.contains_key("session"),keep);
        assert!(registry.bindings.get("session").expect("binding").tainted_reason.is_none());
        registry.close_all().await.expect("reaped");
    }
}
