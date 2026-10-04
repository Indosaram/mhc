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
async fn persisted_binding_reattaches_only_with_verified_transcript() {
    use maho_ext_anthropic_subscription::{session_continuity::Snapshot,session_sync};
    for transcript in ["verified","missing","orphan"] {
        let verified=transcript=="verified";
        let directory=tempfile::tempdir().expect("directory");let executable=directory.path().join("claude");
        std::fs::write(&executable,"#!/usr/bin/python3\nimport json,sys\nfor line in sys.stdin:\n f=json.loads(line)\n if f['type']=='control_request': print(json.dumps({'type':'control_response','response':{'subtype':'success','request_id':f['request_id'],'response':{}}}),flush=True)\n else:\n  print(json.dumps({'type':'user','isReplay':True,'uuid':f['uuid']}),flush=True)\n  print(json.dumps({'type':'result','subtype':'success','user_message_uuid':f['uuid']}),flush=True)\n").expect("script");
        std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o700)).expect("permissions");
        let options=json!({"cwd":directory.path(),"systemPrompt":"original"});let previous=json!({"messages":[{"role":"user","content":"previous"}]});
        let hashes=session_sync::sent_message_hashes(&session_sync::sent_messages(&previous));let fingerprint=session_sync::config_fingerprint(&options,&previous,"ambient","default");
        let sdk="00000000-0000-0000-0000-000000000001";
        let config=directory.path().join("config");let project=config.join("projects").join("fixture-project");std::fs::create_dir_all(&project).expect("project");
        if transcript!="missing" {
            let mut text=format!("{{\"type\":\"assistant\",\"uuid\":\"anchor\",\"sessionId\":\"{sdk}\",\"message\":{{\"content\":[]}}}}\n");
            if transcript=="orphan" {text.push_str(&format!("{{\"type\":\"user\",\"uuid\":\"orphan\",\"parentUuid\":\"anchor\",\"sessionId\":\"{sdk}\",\"message\":{{\"content\":\"uncommitted\"}}}}\n"));}
            std::fs::write(project.join(format!("{sdk}.jsonl")),text).expect("transcript");
        }
        let environment=BTreeMap::from([("CLAUDE_CONFIG_DIR".into(),config.to_string_lossy().into_owned()),("CLAUDE_CODE_PROJECT_DIR_NAME".into(),"fixture-project".into())]);let custom=BTreeMap::new();let mut registry=SessionRegistry::default();
        registry.bindings.remember("session",&Snapshot {sdk_session_id:sdk.into(),account_name:"default".into(),model_id:"model".into(),system_prompt_hash:fingerprint.system_prompt_hash,toolset_hash:fingerprint.toolset_hash,sent_count:1,sent_hashes:Vec::new(),sent_prefix_hash:Some(session_sync::sent_hash_prefix_digest(&hashes,1)),last_assistant_uuid:Some("anchor".into()),sdk_session_id_confirmed:Some(true),..Default::default()});
        let context=json!({"messages":[{"role":"user","content":"previous"},{"role":"user","content":"next"}]});
        tokio::time::timeout(std::time::Duration::from_secs(10),registry.turn(ResidentInput {session:"session",account:"default",model:"model",context:&context,options:&options,executable:&executable,environment:&environment,auth_lane:"ambient",custom:&custom,tool_note:None,now:1,transcript_available:true,signal:None},|_|{})).await.expect("bounded turn").expect("turn");
        assert_eq!(registry.last_decisions["session"].reason,if verified {"registry_miss"}else {"transcript_missing"});
        assert_eq!(registry.last_decisions["session"].delta_messages,if verified {1}else {2});
        assert_eq!(registry.entries["session"].pump.sdk_session_id==sdk,verified);
        registry.close_all().await.expect("cleanup");
    }
}

#[tokio::test]
async fn interrupt_receipt_controls_keep_or_close_without_taint() {
    for (receipt, keep) in [("{'still_queued':[]}",true),("{'still_queued':['pending']}",false),("{}",false),("{'still_queued':None}",false)] {
        let directory = tempfile::tempdir().expect("dir"); let executable = directory.path().join("claude");
        std::fs::write(&executable, format!("#!/usr/bin/python3\nimport json,sys\nu=None\nfor line in sys.stdin:\n f=json.loads(line)\n if f['type']=='control_request':\n  r={receipt} if f['request']['subtype']=='interrupt' else {{}}\n  print(json.dumps({{'type':'control_response','response':{{'subtype':'success','request_id':f['request_id'],'response':r}}}}),flush=True)\n  if f['request']['subtype']=='interrupt': print(json.dumps({{'type':'result','subtype':'success','user_message_uuid':u}}),flush=True)\n else:\n  u=f['uuid']\n  print(json.dumps({{'type':'user','isReplay':True,'uuid':u}}),flush=True)\n  print(json.dumps({{'type':'stream_event','event':{{'type':'message_start'}}}}),flush=True)\n")).expect("script");
        std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o700)).expect("permissions");
        let environment = BTreeMap::new(); let custom = BTreeMap::new(); let mut registry = SessionRegistry::default();
        let context = json!({"messages":[{"role":"user","content":"interrupt"}]}); let options = json!({"cwd":directory.path()});
        let controller = maho_ai::utils::abort::AbortController::new(); let signal = controller.signal();
        let result = tokio::time::timeout(std::time::Duration::from_secs(10),registry.turn(ResidentInput { session:"session",account:"default",model:"model",context:&context,options:&options,executable:&executable,environment:&environment,auth_lane:"ambient",custom:&custom,tool_note:None,now:1,transcript_available:true,signal:Some(&signal) }, |message| {
            if message["type"] == "stream_event" { controller.abort(None); }
            assert_ne!(message["type"], "result");
        })).await.expect("bounded interrupt").expect("interrupted turn");
        assert!(result.aborted);
        assert_eq!(registry.entries.contains_key("session"),keep,"receipt {receipt}");
        assert!(registry.bindings.get("session").expect("binding").tainted_reason.is_none());
        assert!(registry.bindings.get("session").expect("retained binding").sdk_session_id_confirmed==Some(true));
        if keep {assert!(registry.entries["session"].evictable());}
        else {assert!(registry.bindings.get("session").expect("retry checkpoint").unanswered_turn_digest.is_some());}
        registry.close_all().await.expect("reaped");
    }
}

#[tokio::test]
async fn incomplete_interrupt_completion_expires_grace_and_closes_child() {
    for receipt in ["", "  if f['request']['subtype']=='interrupt': print(json.dumps({'type':'control_response','response':{'subtype':'success','request_id':f['request_id'],'response':{'still_queued':[]}}}),flush=True)\n", "  if f['request']['subtype']=='interrupt': print(json.dumps({'type':'control_response','response':{'subtype':'error','request_id':f['request_id'],'error':'interrupt refused'}}),flush=True)\n"] {
    let directory=tempfile::tempdir().expect("directory");let executable=directory.path().join("claude");
    let script="#!/usr/bin/python3\nimport json,sys\nfor line in sys.stdin:\n f=json.loads(line)\n if f['type']=='control_request':\n  if f['request']['subtype']=='initialize': print(json.dumps({'type':'control_response','response':{'subtype':'success','request_id':f['request_id'],'response':{}}}),flush=True)\n".to_owned()+receipt+" else:\n  print(json.dumps({'type':'user','isReplay':True,'uuid':f['uuid']}),flush=True)\n  print(json.dumps({'type':'stream_event','event':{'type':'message_start'}}),flush=True)\n";
    std::fs::write(&executable,script).expect("script");
    std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o700)).expect("permissions");
    let environment=BTreeMap::new();let custom=BTreeMap::new();let mut registry=SessionRegistry::default();
    let context=json!({"messages":[{"role":"user","content":"interrupt"}]});let options=json!({"cwd":directory.path()});
    let controller=maho_ai::utils::abort::AbortController::new();let signal=controller.signal();
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),registry.turn(ResidentInput {session:"session",account:"default",model:"model",context:&context,options:&options,executable:&executable,environment:&environment,auth_lane:"ambient",custom:&custom,tool_note:None,now:1,transcript_available:true,signal:Some(&signal)},|message| {
        if message["type"]=="stream_event" {controller.abort(None);}
        assert_ne!(message["type"],"result");
    })).await.expect("bounded interrupt grace").expect("aborted completion");
    assert!(result.aborted);
    assert!(registry.entries.is_empty(),"a child without a completed interrupted turn cannot be retained");
    assert!(registry.reapers.is_empty());
    let binding=registry.bindings.get("session").expect("uncertain abort retains lineage");
    assert!(binding.tainted_reason.is_none());
    assert!(binding.unanswered_turn_digest.is_some());
    registry.close_all().await.expect("cleanup");
    }
}

#[tokio::test]
async fn abort_from_stream_callback_wins_over_already_queued_result() {
    let directory=tempfile::tempdir().expect("directory");let executable=directory.path().join("claude");
    std::fs::write(&executable,"#!/usr/bin/python3\nimport json,sys\nfor line in sys.stdin:\n f=json.loads(line)\n if f['type']=='control_request':\n  print(json.dumps({'type':'control_response','response':{'subtype':'success','request_id':f['request_id'],'response':{'still_queued':[]}}}),flush=True)\n else:\n  frames=[{'type':'user','isReplay':True,'uuid':f['uuid']},{'type':'stream_event','event':{'type':'message_start'}},{'type':'result','subtype':'success','user_message_uuid':f['uuid']} ]\n  sys.stdout.write(''.join(json.dumps(frame)+'\\n' for frame in frames));sys.stdout.flush()\n").expect("script");
    std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o700)).expect("permissions");
    let environment=BTreeMap::new();let custom=BTreeMap::new();let mut registry=SessionRegistry::default();
    let context=json!({"messages":[{"role":"user","content":"interrupt"}]});let options=json!({"cwd":directory.path()});
    let controller=maho_ai::utils::abort::AbortController::new();let signal=controller.signal();
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),registry.turn(ResidentInput {session:"session",account:"default",model:"model",context:&context,options:&options,executable:&executable,environment:&environment,auth_lane:"ambient",custom:&custom,tool_note:None,now:1,transcript_available:true,signal:Some(&signal)},|message| {
        if message["type"]=="stream_event" {controller.abort(None);}
        assert_ne!(message["type"],"result","an aborted turn cannot publish a success result");
    })).await.expect("bounded abort").expect("completion");
    assert!(result.aborted);
    assert!(registry.entries["session"].evictable());
    registry.close_all().await.expect("cleanup");
}
