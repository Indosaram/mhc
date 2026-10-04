use maho_core::agent_session::{AgentSession,QueuedInputOptions};
use crate::rpc_types::{RpcCommand,RpcCommandBody,RpcResponse,RpcResponseResult,ResponseRecordType};
pub fn json_parse_error_message(input:&str)->String{
    struct Parser<'a>{bytes:&'a [u8],at:usize}
    impl Parser<'_>{
        fn space(&mut self){while self.bytes.get(self.at).is_some_and(|byte|matches!(byte,b' '|b'\n'|b'\r'|b'\t')){self.at+=1;}}
        fn string(&mut self)->Result<(),String>{
            self.at+=1;
            while let Some(&byte)=self.bytes.get(self.at){
                self.at+=1;
                match byte{
                    b'"'=>return Ok(()),
                    0..=31=>return Err("Unterminated string".into()),
                    b'\\'=>{
                        let Some(&escape)=self.bytes.get(self.at)else{return Err("Unterminated string".into());};self.at+=1;
                        match escape{
                            b'"'|b'\\'|b'/'|b'b'|b'f'|b'n'|b'r'|b't'=>{},
                            b'u'=>{
                                let digits=&self.bytes[self.at..self.bytes.len().min(self.at+4)];
                                if digits.len()<4||digits.contains(&b'"'){return Err("\\u must be followed by 4 hex digits".into());}
                                if !digits.iter().all(u8::is_ascii_hexdigit){return Err(format!("\"\\u{}\" is not a valid unicode escape",String::from_utf8_lossy(digits)));}self.at+=4;
                            },
                            _=>return Err(format!("Invalid escape character {}",String::from_utf8_lossy(&self.bytes[self.at-1..]).chars().next().unwrap_or_default())),
                        }
                    },_=>{}
                }
            }
            Err("Unterminated string".into())
        }
        fn value(&mut self)->Result<(),String>{
            self.space();let Some(&byte)=self.bytes.get(self.at)else{return Err("Unexpected EOF".into());};
            match byte{
                b'"'=>self.string(),
                b'-'|b'0'..=b'9'=>{
                    if byte==b'-'{self.at+=1;}
                    match self.bytes.get(self.at){Some(b'0')=>self.at+=1,Some(b'1'..=b'9')=>{while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit){self.at+=1;}},_=>return Err("Invalid number".into())}
                    if self.bytes.get(self.at)==Some(&b'.'){self.at+=1;if !self.bytes.get(self.at).is_some_and(u8::is_ascii_digit){return Err("Invalid digits after decimal point".into());}while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit){self.at+=1;}}
                    if self.bytes.get(self.at).is_some_and(|byte|matches!(byte,b'e'|b'E')){
                        self.at+=1;
                        if self.bytes.get(self.at).is_some_and(|byte|matches!(byte,b'+'|b'-')){self.at+=1;}
                        if !self.bytes.get(self.at).is_some_and(u8::is_ascii_digit){return Err("Unable to parse JSON string".into());}
                        while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit){self.at+=1;}
                    }
                    Ok(())
                },
                byte if byte.is_ascii_alphabetic()=>{
                    for literal in [b"true".as_slice(),b"false",b"null"]{if self.bytes[self.at..].starts_with(literal){self.at+=literal.len();return Ok(());}}
                    let start=self.at;while self.bytes.get(self.at).is_some_and(|byte|byte.is_ascii_alphanumeric()||*byte==b'_'){self.at+=1;}
                    Err(format!("Unexpected identifier \"{}\"",String::from_utf8_lossy(&self.bytes[start..self.at])))
                },b'}'|b']'|b','|b':'=>Err(format!("Unexpected token '{}'",char::from(byte))),_=>Err(format!("Unrecognized token '{}'",String::from_utf8_lossy(&self.bytes[self.at..]).chars().next().unwrap_or_default()))
            }
        }
        fn document(&mut self)->Result<(),String>{
            let mut containers=Vec::new();
            loop{
                self.space();
                match self.bytes.get(self.at){
                    Some(b'{')=>{
                        self.at+=1;self.space();
                        if self.bytes.get(self.at)==Some(&b'}'){self.at+=1;}else{
                            if self.bytes.get(self.at)!=Some(&b'"'){return Err("Expected '}'".into());}
                            self.string()?;self.space();if self.bytes.get(self.at)!=Some(&b':'){return Err("Expected ':' before value in object property definition".into());}self.at+=1;containers.push(b'}');continue;
                        }
                    },
                    Some(b'[')=>{self.at+=1;self.space();if self.bytes.get(self.at)==Some(&b']'){self.at+=1;}else{containers.push(b']');continue;}},
                    _=>self.value()?,
                }
                loop{
                    self.space();let Some(&closing)=containers.last()else{return if self.at==self.bytes.len(){Ok(())}else{Err("Unable to parse JSON string".into())};};
                    if self.bytes.get(self.at)==Some(&closing){self.at+=1;containers.pop();continue;}
                    if self.bytes.get(self.at)!=Some(&b','){return Err(format!("Expected '{}'",char::from(closing)));}
                    self.at+=1;self.space();
                    if closing==b']'{if self.bytes.get(self.at)==Some(&b']'){return Err("Unexpected comma at the end of array expression".into());}}else{
                        if self.bytes.get(self.at)!=Some(&b'"'){return Err("Property name must be a string literal".into());}
                        self.string()?;self.space();if self.bytes.get(self.at)!=Some(&b':'){return Err("Expected ':' before value in object property definition".into());}self.at+=1;
                    }
                    break;
                }
            }
        }
    }
    let mut parser=Parser{bytes:input.as_bytes(),at:0};
    let result=parser.document();
    format!("JSON Parse error: {}",result.err().unwrap_or_else(||"Unable to parse JSON string".into()))
}
pub fn json_parse_error_code_units(input:&str)->Vec<u16>{
    let message=json_parse_error_message(input);
    let mut units=message.encode_utf16().collect::<Vec<_>>();
    // JavaScriptCore names an unexpected token by its first UTF-16 code unit,
    // including a lone high surrogate for an astral character.
    if (message.starts_with("JSON Parse error: Unrecognized token '")||message.starts_with("JSON Parse error: Invalid escape character "))&&let Some(index)=units.iter().position(|unit|(0xd800..=0xdbff).contains(unit))&&units.get(index+1).is_some_and(|unit|(0xdc00..=0xdfff).contains(unit)){
        units.remove(index+1);
    }
    units
}
pub fn json_parse_error_response(input:&str)->String{
    use std::fmt::Write;
    let mut error=String::from("Failed to parse command: ").encode_utf16().collect::<Vec<_>>();
    error.extend(json_parse_error_code_units(input));
    let mut response=String::from("{\"type\":\"response\",\"command\":\"parse\",\"success\":false,\"error\":\"");
    for unit in error{let _=write!(response,"\\u{unit:04x}");}
    response.push_str("\"}\n");response
}
pub type RpcRecordSink=std::sync::Arc<dyn Fn(serde_json::Value)+Send+Sync>;
pub async fn handle_input_line(session:&AgentSession,line:&str)->Result<Option<String>,serde_json::Error>{
    handle_input_line_with_sink(session,line,None).await
}
pub async fn handle_input_line_with_sink(session:&AgentSession,line:&str,sink:Option<RpcRecordSink>)->Result<Option<String>,serde_json::Error>{
    let parsed=serde_json::from_str::<serde_json::Value>(line);
    if parsed.is_err(){return Ok(Some(json_parse_error_response(line)));}
    let error=match &parsed{Err(_)=>None,Ok(value)=>crate::rpc_input_validation::rpc_command_shape_error(value).map(str::to_owned)};
    if let Some(error)=error{return crate::jsonl::serialize_json_line(&RpcResponse{id:None,record_type:ResponseRecordType::Response,command:"parse".into(),session_id:None,result:RpcResponseResult::Error{error,error_code:None,error_data:None}}).map(Some);}
    let value=parsed?;
    let error=crate::rpc_input_validation::rpc_command_payload_error(&value).map(str::to_owned).or_else(||crate::rpc_input_validation::rpc_message_length_error(&value));
    if let Some(error)=error{return crate::jsonl::serialize_json_line(&RpcResponse{id:value["id"].as_str().map(str::to_owned),record_type:ResponseRecordType::Response,command:value["type"].as_str().unwrap_or_default().into(),session_id:None,result:RpcResponseResult::Error{error,error_code:None,error_data:None}}).map(Some);}
    let command:RpcCommand=match serde_json::from_value(value.clone()){
        Ok(command)=>command,
        Err(error) if error.to_string().starts_with(&format!("unknown variant `{}`,",value["type"].as_str().unwrap_or_default()))=>{let kind=value["type"].as_str().unwrap_or_default();return crate::jsonl::serialize_json_line(&RpcResponse{id:value.get("id").and_then(serde_json::Value::as_str).map(str::to_owned),record_type:ResponseRecordType::Response,command:kind.into(),session_id:value.get("sessionId").and_then(serde_json::Value::as_str).map(str::to_owned),result:RpcResponseResult::Error{error:format!("Unknown command: {kind}"),error_code:None,error_data:None}}).map(Some);},
        Err(error)=>return Err(error),
    };
    handle_session_command_with_sink(session,&command,sink).await.map(|response|crate::jsonl::serialize_json_line(&response)).transpose()
}
/// The wire shape of one remembered model entry: the model plus its remembered thinking
/// level/selection and service tier (senpi's `RpcSessionModelEntry` spread).
pub fn session_model_entry_json(entry:&maho_core::agent_session::SessionModelEntry)->serde_json::Value{
    let mut value=serde_json::json!({"model":&entry.model});
    if let Some(level)=&entry.thinking_level{value["thinkingLevel"]=serde_json::to_value(level).unwrap_or(serde_json::Value::Null);}
    if let Some(selection)=&entry.thinking_selection{if let Ok(selection)=serde_json::to_value(selection){value["thinkingSelection"]=selection;}}
    if let Some(tier)=entry.service_tier{value["serviceTier"]=match tier{maho_ext_api::ServiceTier::Auto=>"auto",maho_ext_api::ServiceTier::Flex=>"flex",maho_ext_api::ServiceTier::Priority=>"priority"}.into();}
    value
}
/// Milliseconds since the epoch, the clock the RPC host stamps records with.
pub fn now_ms()->f64{std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.,|elapsed|elapsed.as_secs_f64()*1000.)}
/// Project one session into the wire state shape (senpi `buildRpcSessionState`).
///
/// Shared with `open_session` so both surfaces answer with the SAME fields, instead of a
/// second hand-rolled literal that silently drifts.
pub fn build_rpc_session_state(session:&AgentSession,last_abort_source:Option<&str>)->serde_json::Value{
    // senpi's `buildRpcSessionState` consults the project-trust store directly (not the settings
    // manager's cached copy) and always publishes the session manager's accumulated usage totals.
    let project_trusted=maho_core::ProjectTrustStore::new(&session.agent_dir()).get(&session.cwd()).is_ok_and(|decision|decision==Some(true));
    let usage_totals=session.with_session_manager(|manager|manager.usage_totals().clone());
    let mut state=serde_json::json!({
        "model":session.model(),
        "thinkingLevel":session.thinking_level(),
        "serviceTier":session.effective_service_tier().map(|tier|match tier{maho_ext_api::ServiceTier::Auto=>"auto",maho_ext_api::ServiceTier::Flex=>"flex",maho_ext_api::ServiceTier::Priority=>"priority"}),
        "fastMode":session.is_fast_mode_active(),
        "isStreaming":session.is_streaming(),
        "isCompacting":session.is_compacting(),
        "retryAttempt":session.retry_attempt(),
        "isBashRunning":session.is_bash_running(),
        "steeringMode":match session.steering_mode(){maho_agent::types::QueueMode::All=>"all",maho_agent::types::QueueMode::OneAtATime=>"one-at-a-time"},
        "followUpMode":match session.follow_up_mode(){maho_agent::types::QueueMode::All=>"all",maho_agent::types::QueueMode::OneAtATime=>"one-at-a-time"},
        "sessionFile":session.session_file(),
        "sessionId":session.session_id(),
        "sessionName":session.session_name(),
        "cwd":session.cwd(),
        "projectTrusted":project_trusted,
        "usageTotals":{"input":usage_totals.input,"output":usage_totals.output,"cacheRead":usage_totals.cache_read,"cacheWrite":usage_totals.cache_write,"cost":usage_totals.cost},
        "autoCompactionEnabled":session.auto_compaction_enabled(),
        "messageCount":session.messages().len(),
        "pendingMessageCount":session.pending_message_count(),
        "steering":session.get_steering_messages(),
        "followUp":session.get_follow_up_messages(),
        "ordered":session.get_queued_input_order().iter().map(|input|serde_json::json!({"text":input.text,"mode":match input.mode{maho_ext_api::StreamingBehavior::Steer=>"steer",maho_ext_api::StreamingBehavior::FollowUp=>"followUp"},"enqueueOrder":input.enqueue_order})).collect::<Vec<_>>(),
        "favoriteModels":session.favorite_models().iter().map(session_model_entry_json).collect::<Vec<_>>(),
        "scopedModels":session.scoped_models().iter().map(session_model_entry_json).collect::<Vec<_>>(),
    });
    if let Some(thinking_selection)=session.thinking_selection(){if let Ok(value)=serde_json::to_value(thinking_selection){state["thinkingSelection"]=value;}}
    if let Some(abort_source)=last_abort_source{state["lastAbortSource"]=abort_source.into();}
    if let Some(context_usage)=session.get_context_usage(){state["contextUsage"]=serde_json::json!({"tokens":context_usage.tokens,"contextWindow":context_usage.context_window,"percent":context_usage.percent});}
    // Entries are published only while the session has no file on disk yet: a client that misses
    // them can never reconstruct the session it is bound to (senpi's existsSync guard).
    let session_file=session.session_file();
    if session_file.as_deref().is_none_or(|file|!std::path::Path::new(file).exists()){
        let entries=session.with_session_manager(|manager|manager.entries());
        if entries.iter().any(|entry|!matches!(entry["type"].as_str(),Some("model_change"|"model_change_rejected"|"thinking_level_change"))){state["entries"]=serde_json::json!(entries);}
    }
    state
}
pub async fn handle_session_command(session:&AgentSession,command:&RpcCommand)->Option<RpcResponse>{
    handle_session_command_with_sink(session,command,None).await
}
async fn handle_session_command_with_sink(session:&AgentSession,command:&RpcCommand,sink:Option<RpcRecordSink>)->Option<RpcResponse>{
    let (kind,result)=match &command.body{
        RpcCommandBody::Prompt{message,images,streaming_behavior,thinking_level,session_title_prompt,expand_prompt_templates}=>{
            let options=(||{
                if thinking_level.is_some()&&session.is_streaming()&&streaming_behavior.is_some(){return Err("Cannot set thinkingLevel on a queued prompt; set it after the current turn completes.".to_owned());}
                let images=images.as_ref().map(|images|images.iter().cloned().map(serde_json::from_value).collect::<Result<Vec<maho_ai::types::ImageContent>,_>>()).transpose().map_err(|error|error.to_string())?;
                let thinking_level=thinking_level.as_ref().map(|level|serde_json::from_value(level.clone().into())).transpose().map_err(|error|error.to_string())?;
                let session_title_prompt=match session_title_prompt{Some(crate::rpc_types::SessionTitlePrompt::Text(text))=>Some(maho_core::agent_session::SessionTitlePrompt::Text(text.clone())),Some(crate::rpc_types::SessionTitlePrompt::Enabled(false))=>Some(maho_core::agent_session::SessionTitlePrompt::Disabled),None=>None,Some(crate::rpc_types::SessionTitlePrompt::Enabled(true))=>return Err("sessionTitlePrompt must be a string or false".into())};
                Ok(maho_core::agent_session::PromptOptions{images,thinking_level,session_title_prompt,expand_prompt_templates:*expand_prompt_templates,streaming_behavior:streaming_behavior.map(|behavior|match behavior{crate::rpc_types::StreamingBehavior::Steer=>maho_ext_api::StreamingBehavior::Steer,crate::rpc_types::StreamingBehavior::FollowUp=>maho_ext_api::StreamingBehavior::FollowUp}),source:Some(maho_ext_api::InputSource::Rpc),..Default::default()})
            })();
            let admitted=std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let result=match options{Err(error)=>Err(error),Ok(mut options)=>{
                if let Some(sink)=sink{
                    let admitted=admitted.clone();let id=command.id.clone();let session_id=command.session_id.clone();
                    options.prompt_admitted=Some(std::sync::Arc::new(move|disposition|{
                        if admitted.swap(true,std::sync::atomic::Ordering::SeqCst){return;}
                        let disposition=match disposition{maho_core::agent_session::PromptDisposition::Started=>"started",maho_core::agent_session::PromptDisposition::Queued=>"queued",maho_core::agent_session::PromptDisposition::Handled=>"handled"};
                        let mut record=serde_json::json!({"type":"response","command":"prompt","success":true,"data":{"disposition":disposition}});
                        if let Some(id)=&id{record["id"]=id.clone().into();}if let Some(session_id)=&session_id{record["sessionId"]=session_id.clone().into();}
                        sink(record);
                    }));
                }
                session.prompt(message,options).await.map(|disposition|Some(serde_json::json!({"disposition":match disposition{maho_core::agent_session::PromptDisposition::Started=>"started",maho_core::agent_session::PromptDisposition::Queued=>"queued",maho_core::agent_session::PromptDisposition::Handled=>"handled"}})))
            }};
            if admitted.load(std::sync::atomic::Ordering::SeqCst){return None;}
            ("prompt",result)
        },
        RpcCommandBody::SwitchSession{session_path,cwd_override}=>
            ("switch_session",session.switch_session_with_cwd(session_path,cwd_override.as_deref()).await.map(|switched|Some(serde_json::json!({"cancelled":!switched})))),
        RpcCommandBody::Abort=>{
            let mut abort=Box::pin(session.abort());
            let completed=std::future::poll_fn(|context|std::task::Poll::Ready(abort.as_mut().poll(context).is_ready())).await;
            if !completed&&let Some(sink)=sink{
                let mut record=serde_json::json!({"type":"response","command":"abort","success":true});
                if let Some(id)=&command.id{record["id"]=id.clone().into();}if let Some(id)=&command.session_id{record["sessionId"]=id.clone().into();}
                sink(record);abort.await;return None;
            }
            if !completed{abort.await;}
            ("abort",Ok(None))
        },
        RpcCommandBody::Reload=>("reload",session.reload().await.map(|result|Some(serde_json::json!({"cancelled":!result})))),
        RpcCommandBody::SetFavoriteModels{models}|RpcCommandBody::SetScopedModels{models}=>{
            #[derive(serde::Deserialize)]#[serde(rename_all="camelCase")]struct WireModel{model:maho_ai::types::Model,thinking_level:Option<maho_ai::types::ThinkingLevel>,thinking_selection:Option<maho_ai::types::ThinkingSelection>,service_tier:Option<String>}
            let parsed=models.iter().cloned().map(|value|{
                let entry:WireModel=serde_json::from_value(value).map_err(|error|error.to_string())?;
                let service_tier=entry.service_tier.map(|tier|match tier.as_str(){"auto"=>Ok(maho_ext_api::ServiceTier::Auto),"flex"=>Ok(maho_ext_api::ServiceTier::Flex),"priority"=>Ok(maho_ext_api::ServiceTier::Priority),_=>Err(format!("Unknown service tier: {tier}"))}).transpose()?;
                Ok(maho_core::agent_session::SessionModelEntry{model:entry.model,thinking_level:entry.thinking_level,thinking_selection:entry.thinking_selection,service_tier})
            }).collect::<Result<Vec<_>,String>>();
            let favorite=matches!(&command.body,RpcCommandBody::SetFavoriteModels{..});
            (if favorite{"set_favorite_models"}else{"set_scoped_models"},parsed.map(|models|{if favorite{session.set_favorite_models(models);}else{session.set_scoped_models(models);}None}))
        },
        RpcCommandBody::AppendSessionEntry{entry}=>{session.append_session_entry(entry.clone());("append_session_entry",Ok(None))},
        RpcCommandBody::GetFastMode=>("get_fast_mode",Ok(Some(serde_json::json!({"enabled":session.is_fast_mode_active(),"serviceTier":session.effective_service_tier().map(|tier|match tier{maho_ext_api::ServiceTier::Auto=>"auto",maho_ext_api::ServiceTier::Flex=>"flex",maho_ext_api::ServiceTier::Priority=>"priority"})})))),
        RpcCommandBody::CleanupBashOutput{path}=>("cleanup_bash_output",session.cleanup_bash_output(std::path::Path::new(path)).await.map(|()|None)),
        RpcCommandBody::ExportJsonl{output_path}=>("export_jsonl",session.export_to_jsonl(output_path.as_deref()).map(|path|Some(serde_json::json!({"path":path}))).map_err(|error|error.to_string())),
        RpcCommandBody::Steer{message,images,enqueue_order}|RpcCommandBody::FollowUp{message,images,enqueue_order}=>{
            let kind=if matches!(&command.body,RpcCommandBody::Steer{..}){"steer"}else{"follow_up"};
            let images=images.as_ref().map(|images|images.iter().cloned().map(serde_json::from_value).collect::<Result<Vec<maho_ai::types::ImageContent>,_>>()).transpose();
            let result=match images{Err(error)=>Err(error.to_string()),Ok(images)=>{
                let options=QueuedInputOptions{enqueue_order:enqueue_order.map(|order|order as u64),source:Some(maho_ext_api::InputSource::Rpc)};
                if kind=="steer"{session.steer(message,images,options).await}else{session.follow_up(message,images,options).await}
            }};
            (kind,result.map(|()|None))
        },
        RpcCommandBody::GetSteeringMessages=>("get_steering_messages",Ok(Some(serde_json::json!({"messages":session.get_steering_messages()})))),
        RpcCommandBody::GetFollowUpMessages=>("get_follow_up_messages",Ok(Some(serde_json::json!({"messages":session.get_follow_up_messages()})))),
        RpcCommandBody::ClearQueue{abort_will_follow}=>{let cleared=session.clear_queue(abort_will_follow.unwrap_or(false));let ordered=cleared.ordered.iter().map(|input|serde_json::json!({"text":input.text,"mode":match input.mode{maho_ext_api::StreamingBehavior::Steer=>"steer",maho_ext_api::StreamingBehavior::FollowUp=>"followUp"},"enqueueOrder":input.enqueue_order})).collect::<Vec<_>>();("clear_queue",Ok(Some(serde_json::json!({"steering":cleared.steering,"followUp":cleared.follow_up,"ordered":ordered}))))},
        RpcCommandBody::SetSessionName{name}=>{let name=name.trim();let result=if name.is_empty(){Err("Session name cannot be empty".into())}else{session.set_session_name(name);Ok(None)};("set_session_name",result)},
        RpcCommandBody::GetLastAssistantText=>("get_last_assistant_text",Ok(Some(session.get_last_assistant_text().map_or_else(||serde_json::json!({}),|text|serde_json::json!({"text":text}))))),
        RpcCommandBody::GetMessages=>{let messages=session.messages().into_iter().map(|message|match message{maho_agent::types::AgentMessage::Llm(message)=>serde_json::to_value(message),maho_agent::types::AgentMessage::Custom(message)=>match message{maho_agent::types::CustomAgentMessage::BashExecution(message)=>serde_json::to_value(message),maho_agent::types::CustomAgentMessage::Custom(message)=>serde_json::to_value(message),maho_agent::types::CustomAgentMessage::BranchSummary(message)=>serde_json::to_value(message),maho_agent::types::CustomAgentMessage::CompactionSummary(message)=>serde_json::to_value(message)}}).collect::<Result<Vec<_>,_>>();("get_messages",messages.map(|messages|Some(serde_json::json!({"messages":messages}))).map_err(|error|error.to_string()))},
        RpcCommandBody::SetAutoCompaction{enabled}=>{session.set_auto_compaction_enabled(*enabled);("set_auto_compaction",Ok(None))},
        RpcCommandBody::SetAutoRetry{enabled}=>("set_auto_retry",session.set_auto_retry_enabled(*enabled).map(|()|None)),
        RpcCommandBody::AbortRetry=>{session.abort_retry();("abort_retry",Ok(None))},
        RpcCommandBody::AbortCompaction=>{session.abort_compaction();("abort_compaction",Ok(None))},
        RpcCommandBody::AbortBash=>{session.abort_bash();("abort_bash",Ok(None))},
        RpcCommandBody::SetSteeringMode{mode}|RpcCommandBody::SetFollowUpMode{mode}=>{let mode=match mode{crate::rpc_types::QueueMode::All=>maho_agent::types::QueueMode::All,crate::rpc_types::QueueMode::OneAtATime=>maho_agent::types::QueueMode::OneAtATime};let kind=if matches!(&command.body,RpcCommandBody::SetSteeringMode{..}){session.set_steering_mode(mode);"set_steering_mode"}else{session.set_follow_up_mode(mode);"set_follow_up_mode"};(kind,Ok(None))},
        RpcCommandBody::GetAvailableThinkingLevels=>("get_available_thinking_levels",Ok(Some(serde_json::json!({"levels":session.get_available_thinking_levels()})))),
        RpcCommandBody::CycleThinkingLevel=>("cycle_thinking_level",Ok(Some(session.cycle_thinking_level().map_or(serde_json::Value::Null,|level|serde_json::json!({"level":level}))))),
        RpcCommandBody::SetThinkingLevel{level,scope}=>{
            let result=serde_json::from_value::<maho_ai::types::ModelThinkingLevel>(level.clone().into()).map_err(|error|error.to_string()).and_then(|parsed|{
                if scope.is_some(){if !session.get_available_thinking_levels().contains(&parsed){return Err(format!("Thinking level {level} is not supported by the active model."));}session.set_session_thinking_level(parsed);}else{session.set_thinking_level(parsed);}
                Ok(None)
            });("set_thinking_level",result)
        },
        RpcCommandBody::Compact{custom_instructions}=>("compact",session.compact(custom_instructions.as_deref()).await.map(|result|{
            let mut value=serde_json::json!({"summary":result.summary,"firstKeptEntryId":result.first_kept_entry_id,"tokensBefore":result.tokens_before});
            if let Some(tokens)=result.estimated_tokens_after{value["estimatedTokensAfter"]=tokens.into();}
            if let Some(usage)=result.usage{value["usage"]=serde_json::json!(usage);}
            if let Some(details)=result.details{value["details"]=details;}
            Some(value)
        })),
        RpcCommandBody::GetAvailableModels=>{let models=session.model_registry().get_available().into_iter().map(|model|{let mut value=serde_json::json!(model);value["supportedThinkingLevels"]=serde_json::json!(maho_core::thinking_levels::get_supported_thinking_levels(&model));value}).collect::<Vec<_>>();("get_available_models",Ok(Some(serde_json::json!({"models":models}))))},
        RpcCommandBody::SetModel{provider,model_id}=>{
            let model=session.model_registry().get_available().into_iter().find(|model|&model.provider==provider&&&model.id==model_id);
            let result=if let Some(model)=model{session.set_model(model.clone()).await.map(|change|{let mut value=serde_json::json!(model);if let Some(name)=change.and_then(|change|change.system_prompt_name){value["systemPromptName"]=name.into();}Some(value)})}else{Err(format!("Model not found: {provider}/{model_id}"))};("set_model",result)
        },
        RpcCommandBody::CycleModel{direction}=>("cycle_model",session.cycle_model(!matches!(direction,Some(crate::rpc_types::ModelDirection::Backward))).await.map(|result|Some(result.map_or(serde_json::Value::Null,|result|{
            let mut value=serde_json::json!({"model":result.model,"thinkingLevel":result.thinking_level,"isScoped":result.is_scoped,"skippedModels":result.skipped_models});
            if let Some(change)=result.system_prompt_change{
                let mut event=serde_json::json!({"type":"system_prompt_change","systemPrompt":change.system_prompt,"previousSystemPrompt":change.previous_system_prompt,"model":change.model,"source":"model_select"});
                if let Some(model)=change.previous_model{event["previousModel"]=serde_json::json!(model);}
                if let Some(name)=change.system_prompt_name{event["systemPromptName"]=name.into();}
                value["systemPromptChange"]=event;
            }
            value
        })))),
        RpcCommandBody::GetForkMessages=>{let messages=session.get_user_messages_for_forking().into_iter().map(|(entry_id,text)|serde_json::json!({"entryId":entry_id,"text":text})).collect::<Vec<_>>();("get_fork_messages",Ok(Some(serde_json::json!({"messages":messages}))))},
        RpcCommandBody::GetSessionStats=>{let stats=session.get_session_stats();let mut value=serde_json::json!({"sessionId":stats.session_id,"userMessages":stats.user_messages,"assistantMessages":stats.assistant_messages,"toolCalls":stats.tool_calls,"toolResults":stats.tool_results,"totalMessages":stats.total_messages,"tokens":{"input":stats.tokens.input,"output":stats.tokens.output,"cacheRead":stats.tokens.cache_read,"cacheWrite":stats.tokens.cache_write,"total":stats.tokens.total},"cost":stats.cost});
            if let Some(file)=stats.session_file{value["sessionFile"]=file.into();}
            if let Some(usage)=stats.context_usage{value["contextUsage"]=serde_json::json!({"tokens":usage.tokens,"contextWindow":usage.context_window,"percent":usage.percent});}
            ("get_session_stats",Ok(Some(value)))
        },
        RpcCommandBody::GetEntries{since}=>("get_entries",session.with_session_manager(|manager|{let mut entries=manager.entries();if let Some(since)=since{let index=entries.iter().position(|entry|entry["id"].as_str()==Some(since)).ok_or_else(||format!("Entry not found: {since}"))?;entries.drain(..=index);}Ok(Some(serde_json::json!({"entries":entries,"leafId":manager.leaf_id()})))})),
        RpcCommandBody::SetLabel{entry_id,label}=>{session.with_session_manager_mut(|manager|manager.append_label(entry_id,label.as_deref()));("set_label",Ok(None))},
        RpcCommandBody::RecordBashResult{command,result,exclude_from_context}=>{
            #[derive(serde::Deserialize)]#[serde(rename_all="camelCase")]struct WireResult{output:String,exit_code:Option<i32>,cancelled:bool,truncated:bool,full_output_path:Option<std::path::PathBuf>}
            let response=serde_json::from_value::<WireResult>(result.clone()).map_err(|error|error.to_string()).map(|result|{let parsed=maho_tools::bash_executor::BashResult{output:result.output,exit_code:result.exit_code,cancelled:result.cancelled,truncated:result.truncated,full_output_path:result.full_output_path};session.record_bash_result(command,&parsed,exclude_from_context.unwrap_or(false));None});("record_bash_result",response)
        },
        _=>{let kind=serde_json::to_value(&command.body).ok()?.get("type")?.as_str()?.to_owned();return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:kind.clone(),session_id:command.session_id.clone(),result:RpcResponseResult::Error{error:format!("RPC command requires unfinished runtime binding: {kind}"),error_code:None,error_data:None}});},
    };
    Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:kind.into(),session_id:command.session_id.clone(),result:match result{Ok(data)=>RpcResponseResult::Success{data},Err(error)=>RpcResponseResult::Error{error,error_code:None,error_data:None}}})
}
/// The RPC-side extension UI: dialogs become `extension_ui_request` frames answered by a
/// client's `extension_ui_response`; fire-and-forget methods only emit (senpi's
/// `createExtensionUIContext` inside `createRpcConnectionHandler`).
pub struct RpcExtensionUi{
    out:std::sync::Arc<dyn Fn(serde_json::Value)+Send+Sync>,
    pending:std::sync::Arc<std::sync::Mutex<crate::session_extension_ui_requests::SessionExtensionUiRequests>>,
    questions:std::sync::Arc<std::sync::Mutex<crate::connection_question_bridge::ConnectionQuestionBridge>>,
    capabilities:Vec<String>,
}
impl RpcExtensionUi{
    pub fn new(out:std::sync::Arc<dyn Fn(serde_json::Value)+Send+Sync>,pending:std::sync::Arc<std::sync::Mutex<crate::session_extension_ui_requests::SessionExtensionUiRequests>>,questions:std::sync::Arc<std::sync::Mutex<crate::connection_question_bridge::ConnectionQuestionBridge>>,capabilities:Vec<String>)->Self{Self{out,pending,questions,capabilities}}
    fn ask<T:'static+Send>(&self,opts:&maho_ext_api::ExtensionUiDialogOptions,default:T,mut request:serde_json::Value,parse:impl Fn(serde_json::Value)->T+Send+'static)->maho_ext_api::UiFuture<'static,T>{
        if opts.signal.as_ref().is_some_and(|signal|signal.is_aborted()){return Box::pin(async move{default});}
        let id=uuid::Uuid::new_v4().to_string();
        let(sender,receiver)=tokio::sync::oneshot::channel();
        self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).set(id.clone(),sender);
        request["type"]="extension_ui_request".into();request["id"]=id.clone().into();
        (self.out)(request);
        let timeout=opts.timeout_ms;
        let signal=opts.signal.clone();
        let pending=self.pending.clone();
        Box::pin(async move{
            let mut abort=signal.as_ref().map(|signal|Box::pin(signal.cancelled()));
            let response=match (abort.as_mut(),timeout){
                (Some(abort),Some(ms))=>tokio::select!{received=receiver=>received.ok(),_=abort=>None,()=tokio::time::sleep(std::time::Duration::from_millis(ms))=>None},
                (Some(abort),None)=>tokio::select!{received=receiver=>received.ok(),_=abort=>None},
                (None,Some(ms))=>tokio::select!{received=receiver=>received.ok(),()=tokio::time::sleep(std::time::Duration::from_millis(ms))=>None},
                (None,None)=>receiver.await.ok(),
            };
            pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).delete(&id);
            match response{Some(Ok(response))=>parse(response),Some(Err(_))|None=>default}
        })
    }
}
impl maho_ext_api::ExtensionUi for RpcExtensionUi{
    fn question<'a>(&'a self,request:maho_ext_api::QuestionRequest,options:maho_ext_api::QuestionOptions)->maho_ext_api::ExtensionFuture<'a,maho_ext_api::QuestionResponse>{
        if !self.capabilities.iter().any(|capability|capability==crate::custom_capability::QUESTION_CAPABILITY){
            let ui=self as &dyn maho_ext_api::ExtensionUi;
            return Box::pin(crate::connection_question_bridge::degrade_question(ui,request,options.dialog));
        }
        let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0,|elapsed|elapsed.as_millis() as u64);
        let(frame,receiver)=self.questions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).ask(request.clone(),None,now);
        (self.out)(frame);
        let questions=self.questions.clone();
        Box::pin(async move{
            match tokio::time::timeout(std::time::Duration::from_millis(request.timeout_ms),receiver).await{
                Ok(Ok(response))=>Ok(response),
                _=>{let deadline=now.saturating_add(request.timeout_ms);questions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).expire(deadline);
                    Ok(maho_ext_api::QuestionResponse{status:maho_ext_api::QuestionStatus::TimedOut,answers:Default::default(),comment:None,unanswered:request.questions.iter().map(|question|question.id.clone()).collect(),auto_resolved_after_ms:Some(request.timeout_ms)})}
            }
        })
    }
    fn select<'a>(&'a self,title:&'a str,options:&'a [String],opts:maho_ext_api::ExtensionUiDialogOptions)->maho_ext_api::UiFuture<'a,Option<String>>{
        self.ask(&opts,None,serde_json::json!({"method":"select","title":title,"options":options,"timeout":opts.timeout_ms}),|response|{
            if response["cancelled"].as_bool()==Some(true){None}else{response["value"].as_str().map(str::to_owned)}
        })
    }
    fn confirm<'a>(&'a self,title:&'a str,message:&'a str,opts:maho_ext_api::ExtensionUiDialogOptions)->maho_ext_api::UiFuture<'a,bool>{
        self.ask(&opts,false,serde_json::json!({"method":"confirm","title":title,"message":message,"timeout":opts.timeout_ms}),|response|response["cancelled"].as_bool()!=Some(true)&&response["confirmed"].as_bool().unwrap_or(false))
    }
    fn input<'a>(&'a self,title:&'a str,placeholder:Option<&'a str>,opts:maho_ext_api::ExtensionUiDialogOptions)->maho_ext_api::UiFuture<'a,Option<String>>{
        self.ask(&opts,None,serde_json::json!({"method":"input","title":title,"placeholder":placeholder,"timeout":opts.timeout_ms}),|response|{
            if response["cancelled"].as_bool()==Some(true){None}else{response["value"].as_str().map(str::to_owned)}
        })
    }
    fn editor<'a>(&'a self,title:&'a str,prefill:Option<&'a str>)->maho_ext_api::ExtensionFuture<'a,Option<String>>{
        Box::pin(async move{Ok(self.ask(&maho_ext_api::ExtensionUiDialogOptions::default(),None,serde_json::json!({"method":"editor","title":title,"prefill":prefill}),|response|{
            if response["cancelled"].as_bool()==Some(true){None}else{response["value"].as_str().map(str::to_owned)}
        }).await)})
    }
    fn notify(&self,message:&str,kind:maho_ext_api::NotificationType){
        let kind=match kind{maho_ext_api::NotificationType::Info=>"info",maho_ext_api::NotificationType::Warning=>"warning",maho_ext_api::NotificationType::Error=>"error"};
        (self.out)(serde_json::json!({"type":"extension_ui_request","id":uuid::Uuid::new_v4().to_string(),"method":"notify","message":message,"notifyType":kind}));
    }
    fn set_status(&self,key:&str,text:Option<&str>){(self.out)(serde_json::json!({"type":"extension_ui_request","id":uuid::Uuid::new_v4().to_string(),"method":"setStatus","statusKey":key,"statusText":text}));}
    fn set_widget(&self,key:&str,content:Option<maho_ext_api::WidgetContent>,options:maho_ext_api::ExtensionWidgetOptions){
        let lines=match &content{Some(maho_ext_api::WidgetContent::Lines(lines))=>Some(lines.clone()),_=>None};
        (self.out)(serde_json::json!({"type":"extension_ui_request","id":uuid::Uuid::new_v4().to_string(),"method":"setWidget","widgetKey":key,"widgetLines":lines,"widgetPlacement":options.placement.as_str()}));
    }
    fn set_header(&self,_factory:Option<maho_ext_api::ComponentFactory>){(self.out)(serde_json::json!({"type":"extension_ui_request","id":uuid::Uuid::new_v4().to_string(),"method":"setHeader"}));}
    fn set_footer(&self,_factory:Option<maho_ext_api::ComponentFactory>){(self.out)(serde_json::json!({"type":"extension_ui_request","id":uuid::Uuid::new_v4().to_string(),"method":"setFooter"}));}
    fn set_title(&self,title:&str){(self.out)(serde_json::json!({"type":"extension_ui_request","id":uuid::Uuid::new_v4().to_string(),"method":"setTitle","title":title}));}
    fn paste_to_editor(&self,text:&str){self.set_editor_text(text);}
    fn set_editor_text(&self,text:&str){(self.out)(serde_json::json!({"type":"extension_ui_request","id":uuid::Uuid::new_v4().to_string(),"method":"set_editor_text","text":text}));}
    fn get_editor_text(&self)->String{String::new()}
    fn custom(&self,_factory:maho_ext_api::ComponentFactory,_options:maho_ext_api::CustomUiOptions)->maho_ext_api::ExtensionFuture<'_,serde_json::Value>{
        if let Some(request)=crate::custom_capability::build_custom_unsupported_request(&self.capabilities,crate::custom_capability::DEFAULT_CUSTOM_EXTENSION_LABEL,&uuid::Uuid::new_v4().to_string()){(self.out)(request);}
        Box::pin(async{Ok(serde_json::Value::Null)})
    }
    fn theme(&self)->maho_ext_api::Theme{maho_ext_api::Theme::default()}
}

/**
 * Per-connection RPC handler: one session, one output sink, and the JSONL command loop.
 *
 * It owns exactly one session and speaks the protocol over an injected sink; process signals,
 * stdin wiring and exit belong to the host (senpi `createRpcConnectionHandler`).
 */
pub struct RpcConnectionHandler{
    session:AgentSession,
    ui:std::sync::Arc<RpcExtensionUi>,
    pending:std::sync::Arc<std::sync::Mutex<crate::session_extension_ui_requests::SessionExtensionUiRequests>>,
    questions:std::sync::Arc<std::sync::Mutex<crate::connection_question_bridge::ConnectionQuestionBridge>>,
    shutdown_requested:std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl RpcConnectionHandler{
    /// Build a handler over `session` and install the RPC extension UI on it.
    pub async fn create(session:AgentSession,out:std::sync::Arc<dyn Fn(serde_json::Value)+Send+Sync>,capabilities:Vec<String>)->Self{
        let pending:std::sync::Arc<std::sync::Mutex<crate::session_extension_ui_requests::SessionExtensionUiRequests>>=Default::default();
        let questions:std::sync::Arc<std::sync::Mutex<crate::connection_question_bridge::ConnectionQuestionBridge>>=Default::default();
        let ui=std::sync::Arc::new(RpcExtensionUi::new(out.clone(),pending.clone(),questions.clone(),capabilities));
        let shutdown_requested:std::sync::Arc<std::sync::atomic::AtomicBool>=Default::default();
        let sink=out.clone();
        session.bind_extensions(maho_core::agent_session::ExtensionBindings{ui_context:Some(ui.clone()),mode:Some(maho_ext_api::ExtensionMode::Rpc),on_error:Some(std::sync::Arc::new(move|error:&maho_ext_api::ExtensionError|{(sink)(serde_json::json!({"type":"extension_error","extensionPath":error.extension_path,"event":error.event,"error":error.error}));})),..Default::default()}).await;
        Self{session,ui,pending,questions,shutdown_requested}
    }
    pub fn ui(&self)->&std::sync::Arc<RpcExtensionUi>{&self.ui}
    /// Feed one inbound JSONL line: a UI response settles a pending dialog; anything else is a command.
    pub async fn handle_input_line(&self,line:&str)->Result<Option<String>,serde_json::Error>{
        if let Ok(value)=serde_json::from_str::<serde_json::Value>(line)&&matches!(value["type"].as_str(),Some("extension_ui_response"|"extension_ui_progress")){
            let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0,|elapsed|elapsed.as_millis() as u64);
            let(kind,resolved)=self.questions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).respond(&value,now);
            match kind{crate::connection_question_bridge::QuestionReply::Accepted|crate::connection_question_bridge::QuestionReply::AlreadyResolved=>{if let Some(record)=resolved{self.out_record(record);}}
                crate::connection_question_bridge::QuestionReply::Incomplete=>{},crate::connection_question_bridge::QuestionReply::Unknown=>{self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).resolve(value);}}
            return Ok(None);
        }
        handle_input_line_with_sink(&self.session,line,Some(self.ui.out.clone())).await
    }
    fn out_record(&self,record:serde_json::Value){(self.ui.out)(record);}
    pub fn is_shutdown_requested(&self)->bool{self.shutdown_requested.load(std::sync::atomic::Ordering::SeqCst)}
    /// Cancel UI requests that can no longer be answered by this connection.
    pub fn cancel_pending_extension_ui_requests(&self){self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).cancel_all();self.questions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).cancel_all(0);}
    /// Tear down subscriptions and dispose the session runtime. Never exits the process.
    pub async fn dispose(&self){self.cancel_pending_extension_ui_requests();self.session.emit_session_shutdown(maho_ext_api::SessionReason::Quit).await;self.session.dispose().await;}
}
