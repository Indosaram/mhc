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
type ActiveLogins=std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String,maho_ai::utils::abort::AbortController>>>;
pub async fn handle_input_line(session:&AgentSession,line:&str)->Result<Option<String>,serde_json::Error>{
    handle_input_line_with_sink(session,line,None).await
}
pub async fn handle_input_line_with_sink(session:&AgentSession,line:&str,sink:Option<RpcRecordSink>)->Result<Option<String>,serde_json::Error>{
    handle_input_line_with_login_state(session,line,sink,None,None).await
}
async fn handle_input_line_with_login_state(session:&AgentSession,line:&str,sink:Option<RpcRecordSink>,active_logins:Option<ActiveLogins>,ui:Option<std::sync::Arc<dyn maho_ext_api::ExtensionUi>>)->Result<Option<String>,serde_json::Error>{
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
    handle_session_command_with_sink(session,&command,sink,active_logins,ui).await.map(|response|crate::jsonl::serialize_json_line(&response)).transpose()
}
/// The wire shape of one remembered model entry: the model plus its remembered thinking
/// level/selection and service tier (senpi's `RpcSessionModelEntry` spread).
pub fn session_model_entry_json(entry:&maho_core::agent_session::SessionModelEntry)->serde_json::Value{
    let mut value=serde_json::json!({"model":&entry.model});
    if let Some(level)=&entry.thinking_level{value["thinkingLevel"]=serde_json::to_value(level).unwrap_or(serde_json::Value::Null);}
    if let Some(selection)=&entry.thinking_selection&&let Ok(selection)=serde_json::to_value(selection){value["thinkingSelection"]=selection;}
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
    if let Some(thinking_selection)=session.thinking_selection()&&let Ok(value)=serde_json::to_value(thinking_selection){state["thinkingSelection"]=value;}
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
    handle_session_command_with_sink(session,command,None,None,None).await
}
fn edit_response(session:&AgentSession,result:Result<maho_core::agent_session::AssistantEditResult,String>,not_persisted:&str)->Result<Option<serde_json::Value>,String>{
    let result=result?;
    let leaf_id=session.with_session_manager(|manager|manager.leaf_id().map(str::to_owned));
    if result.unchanged==Some(true){return Ok(Some(serde_json::json!({"outcome":"unchanged","leafId":leaf_id})));}
    if result.cancelled{let mut value=serde_json::json!({"outcome":"cancelled","leafId":leaf_id});if result.aborted==Some(true){value["aborted"]=true.into();}return Ok(Some(value));}
    let entry=result.entry_id.as_deref().and_then(|id|session.with_session_manager(|manager|manager.entry(id)));
    if entry.as_ref().is_none_or(|entry|entry["type"]!="message")||leaf_id.is_none(){return Err(not_persisted.to_owned());}
    let mut value=serde_json::json!({"outcome":"edited","entry":entry,"leafId":leaf_id});
    if let Some(summary)=result.summary_entry.and_then(|entry|entry["id"].as_str().map(str::to_owned)){value["summaryEntryId"]=summary.into();}
    Ok(Some(value))
}
async fn handle_session_command_with_sink(session:&AgentSession,command:&RpcCommand,sink:Option<RpcRecordSink>,active_logins:Option<ActiveLogins>,ui:Option<std::sync::Arc<dyn maho_ext_api::ExtensionUi>>)->Option<RpcResponse>{
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
                        if let Some(id)=&id{record["id"]=id.clone().into();}
                        if let Some(session_id)=&session_id{record["sessionId"]=session_id.clone().into();}
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
        RpcCommandBody::Fork{entry_id,position}=>{
            let include_entry=matches!(position,Some(crate::rpc_types::ForkPosition::At));
            ("fork",session.fork(entry_id,include_entry).await.map(|result|Some(serde_json::json!({"text":result.editor_text,"cancelled":result.cancelled}))))
        },
        RpcCommandBody::Clone=>{
            let leaf_id=session.with_session_manager(|manager|manager.leaf_id().map(str::to_owned));
            let Some(leaf_id)=leaf_id else{return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"clone".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Error{error:"Cannot clone session: no current entry selected".into(),error_code:None,error_data:None}});};
            ("clone",session.fork(&leaf_id,true).await.map(|result|Some(serde_json::json!({"cancelled":result.cancelled}))))
        },
        RpcCommandBody::NewSession{parent_session}=>
            ("new_session",session.new_session(Some(maho_core::session_manager::NewSessionOptions{parent_session:parent_session.clone(),..Default::default()})).await.map(|changed|Some(serde_json::json!({"cancelled":!changed})))),
        RpcCommandBody::Abort=>{
            let mut abort=Box::pin(session.abort());
            let completed=std::future::poll_fn(|context|std::task::Poll::Ready(abort.as_mut().poll(context).is_ready())).await;
            if !completed&&let Some(sink)=sink{
                let mut record=serde_json::json!({"type":"response","command":"abort","success":true});
                if let Some(id)=&command.id{record["id"]=id.clone().into();}
                if let Some(id)=&command.session_id{record["sessionId"]=id.clone().into();}
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
        RpcCommandBody::AppendUserMessage{content}=>{
            let message=serde_json::json!({"role":"user","content":content,"timestamp":maho_ai::utils::diagnostics::now_ms()});
            session.with_session_manager_mut(|manager|{manager.append_message(message);});
            let messages=session.with_session_manager(|manager|manager.build_context(None).messages);
            let agent_messages=messages.iter().filter_map(|message|serde_json::from_value(message.clone()).ok()).collect();
            session.agent().set_messages(agent_messages);
            ("append_user_message",Ok(None))
        },
        RpcCommandBody::SendCustomMessage{custom_type,content,display,details,trigger_turn,deliver_as}=>{
            let content=serde_json::from_value::<maho_agent::harness::messages::CustomMessageContent>(if content.is_null(){serde_json::json!([])}else{content.clone()}).map_err(|error:serde_json::Error|error.to_string());
            let result=match content{Err(error)=>Err(error),Ok(content)=>session.send_custom_message_content(custom_type.clone(),content,*display,details.clone(),maho_ext_api::SendMessageOptions{trigger_turn:trigger_turn.unwrap_or_else(||session.is_streaming()),deliver_as:deliver_as.map(|mode|match mode{crate::rpc_types::DeliveryMode::Steer=>maho_ext_api::DeliverAs::Steer,crate::rpc_types::DeliveryMode::FollowUp=>maho_ext_api::DeliverAs::FollowUp,crate::rpc_types::DeliveryMode::NextTurn=>maho_ext_api::DeliverAs::NextTurn})}).await};
            ("send_custom_message",result.map(|()|None))
        },
        RpcCommandBody::GetFastMode=>("get_fast_mode",Ok(Some(serde_json::json!({"enabled":session.is_fast_mode_active(),"serviceTier":session.effective_service_tier().map(|tier|match tier{maho_ext_api::ServiceTier::Auto=>"auto",maho_ext_api::ServiceTier::Flex=>"flex",maho_ext_api::ServiceTier::Priority=>"priority"})})))),
        RpcCommandBody::SetFastMode{enabled}=>{
            // Same entry point as /fast: persistence, `-fast` key normalization and pin precedence live in apply_fast_mode.
            let result=match session.apply_fast_mode(*enabled).await{
                Ok(result) if !result.applied=>Err(result.message),
                Ok(result)=>{
                    let memory=result.memory_model.unwrap_or_else(||session.model());
                    Ok(Some(serde_json::json!({"enabled":result.enabled,"serviceTier":match result.recorded_tier{maho_ext_api::ServiceTier::Auto=>"auto",maho_ext_api::ServiceTier::Flex=>"flex",maho_ext_api::ServiceTier::Priority=>"priority"},"provider":memory.provider,"modelId":memory.id})))
                }
                Err(error)=>Err(error.message),
            };
            ("set_fast_mode",result)
        },
        RpcCommandBody::CleanupBashOutput{path}=>("cleanup_bash_output",session.cleanup_bash_output(std::path::Path::new(path)).await.map(|()|None)),
        RpcCommandBody::ExportJsonl{output_path}=>("export_jsonl",session.export_to_jsonl(output_path.as_deref()).map(|path|Some(serde_json::json!({"path":path}))).map_err(|error|error.to_string())),
        RpcCommandBody::ExportHtml{output_path,theme_name}=> {
            ("export_html", session.export_to_html(output_path.as_deref(), theme_name.as_deref()).map(|path|Some(serde_json::json!({"path":path}))))
        },
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
        RpcCommandBody::CheckReloadVeto=>("check_reload_veto",session.check_reload_veto().await.map(|value|Some(value.into()))),
        RpcCommandBody::AbortBranchSummary=>{session.abort_branch_summary();("abort_branch_summary",Ok(None))},
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
        RpcCommandBody::GetEntries{since}=>{let since=since.clone();("get_entries",session.with_session_manager(|manager|{let mut entries=manager.entries();if let Some(since)=since.as_deref(){let index=entries.iter().position(|entry|entry["id"].as_str()==Some(since)).ok_or_else(||format!("Entry not found: {since}"))?;entries.drain(..=index);}Ok(Some(serde_json::json!({"entries":entries,"leafId":manager.leaf_id()})))}))},
        RpcCommandBody::NavigateTree{entry_id,target_id,intent,expected_leaf_id,summarize,custom_instructions,replace_instructions,label}=>{
            if entry_id.is_some()&&target_id.is_some(){return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"navigate_tree".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Error{error:"navigate_tree takes either entryId or targetId, not both".into(),error_code:None,error_data:None}});}
            if entry_id.is_none()&&target_id.is_none(){return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"navigate_tree".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Error{error:"navigate_tree requires entryId or targetId".into(),error_code:None,error_data:None}});}
            let selected=entry_id.as_deref().or(target_id.as_deref()).unwrap_or_default();
            if selected.is_empty(){return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"navigate_tree".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Error{error:"navigate_tree requires a non-empty entryId or targetId".into(),error_code:None,error_data:None}});}
            let result=session.navigate_tree(selected,maho_core::agent_session::TreeNavigationOptions{intent:intent.map(|intent|match intent{crate::rpc_types::NavigationIntent::Select=>maho_core::agent_session::TreeNavigationIntent::Select,crate::rpc_types::NavigationIntent::Resume=>maho_core::agent_session::TreeNavigationIntent::Resume}),summarize:*summarize,custom_instructions:custom_instructions.clone(),replace_instructions:*replace_instructions,label:label.clone(),expected_leaf_id:expected_leaf_id.clone()}).await;
            let result=result.map(|result|{let leaf_id=session.with_session_manager(|manager|manager.leaf_id().map(str::to_owned));if target_id.is_some(){let mut value=serde_json::json!({"leafId":leaf_id});if result.cancelled{value["cancelled"]=true.into();}
                if let Some(aborted)=result.aborted{value["aborted"]=aborted.into();}
                if let Some(editor_text)=result.editor_text{value["editorText"]=editor_text.into();}
                if let Some(summary_entry)=result.summary_entry{value["summaryEntry"]=summary_entry;}value}else if result.cancelled{let mut value=serde_json::json!({"outcome":"cancelled","leafId":leaf_id});if let Some(aborted)=result.aborted{value["aborted"]=aborted.into();}value}else{let mut value=serde_json::json!({"outcome":"navigated","leafId":leaf_id});if let Some(editor_text)=result.editor_text{value["editorText"]=editor_text.into();}
                if let Some(summary_entry)=result.summary_entry&&let Some(id)=summary_entry["id"].as_str(){value["summaryEntryId"]=id.into();}value}}).map(Some);("navigate_tree",result)
        },
        RpcCommandBody::SetLabel{entry_id,label}=>{session.with_session_manager_mut(|manager|manager.append_label(entry_id,label.as_deref()));("set_label",Ok(None))},
        RpcCommandBody::Bash{command,exclude_from_context,execution_id,..}=>{
            let result=session.execute_user_bash(command,None,exclude_from_context.unwrap_or(false),execution_id.clone()).await;
            ("bash",result.map(|result|Some(serde_json::to_value(result).unwrap_or(serde_json::Value::Null))))
        },
        RpcCommandBody::RecordBashResult{command,result,exclude_from_context}=>{
            #[derive(serde::Deserialize)]#[serde(rename_all="camelCase")]struct WireResult{output:String,exit_code:Option<i32>,cancelled:bool,truncated:bool,full_output_path:Option<std::path::PathBuf>}
            let response=serde_json::from_value::<WireResult>(result.clone()).map_err(|error|error.to_string()).map(|result|{let parsed=maho_tools::bash_executor::BashResult{output:result.output,exit_code:result.exit_code,cancelled:result.cancelled,truncated:result.truncated,full_output_path:result.full_output_path};session.record_bash_result(command,&parsed,exclude_from_context.unwrap_or(false));None});("record_bash_result",response)
        },
        RpcCommandBody::GetCommands=>{
            // Upstream `case "get_commands"`: `buildRpcCommandsForSession(session)` classifies each of the
            // three groups by its real source; the session's `command_groups` reads the same three.
            let (extensions,templates,skills)=session.command_groups();
            let commands=crate::rpc_command_surface::build_rpc_commands(
                &crate::rpc_command_surface::command_inputs(&extensions),
                &crate::rpc_command_surface::command_inputs(&templates),
                &crate::rpc_command_surface::command_inputs(&skills),
            );
            ("get_commands",Ok(Some(serde_json::json!({"commands":commands}))))
        },
        RpcCommandBody::GetAuthProviders=>{
            let registry=session.model_registry();
            let providers=maho_core::auth_providers::build_login_provider_infos(registry,Some(maho_core::auth_storage::CredentialKind::Oauth)).into_iter().chain(maho_core::auth_providers::build_login_provider_infos(registry,Some(maho_core::auth_storage::CredentialKind::ApiKey))).map(|info|{
                let status=registry.get_provider_auth_status(&info.id);
                serde_json::json!({"id":info.id,"name":info.name,"authType":match info.auth_type{maho_core::auth_storage::CredentialKind::Oauth=>"oauth",maho_core::auth_storage::CredentialKind::ApiKey=>"api_key"},"status":{"configured":status.configured,"source":status.source,"label":status.label}})
            }).collect::<Vec<_>>();
            ("get_auth_providers",Ok(Some(serde_json::json!({"providers":providers}))))
        },
        RpcCommandBody::LoginStart{provider}=>{
            let sink=sink.unwrap_or_else(||std::sync::Arc::new(|_|{}));
            let active_logins=active_logins.unwrap_or_else(||std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())));
            let controller=maho_ai::utils::abort::AbortController::new();
            if let Some(previous)=active_logins.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(provider.clone(),controller.clone()){previous.abort(None);}
            let provider=provider.clone();let storage=session.model_registry().auth_storage.clone();let mut runtime=session.model_registry().model_runtime.clone();let active_logins=active_logins.clone();let event_sink=sink.clone();let ui=ui.unwrap_or_else(||std::sync::Arc::new(crate::connection_handler::RpcExtensionUi::new(event_sink.clone(),Default::default(),Default::default(),Vec::new())));
            tokio::spawn(async move{
                let settled=maho_ai::utils::abort::AbortController::new();
                let combined=maho_ai::utils::abort::AbortController::new();
                let combined_signal=combined.signal();
                let login_signal=controller.signal();
                let settled_signal=settled.signal();
                let combined_for_login=combined.clone();
                let _login_listener=login_signal.add_abort_listener(move |_|combined_for_login.abort(None));
                let combined_for_settled=combined.clone();
                let _settled_listener=settled_signal.add_abort_listener(move |_|combined_for_settled.abort(None));
                let callbacks=std::sync::Arc::new(crate::login_prompts::RpcLoginPromptCallbacks::with_sink(combined_signal,provider.clone(),event_sink.clone(),ui));
                let result=storage.login(&provider,callbacks).await;
                if result.is_ok(){let _=runtime.refresh(Default::default()).await;}
                if provider=="anthropic-subscription"&&result.is_ok(){event_sink(serde_json::json!({"type":"provider_accounts_changed","provider":provider}));}
                let mut end=serde_json::json!({"type":"auth_login_end","provider":provider,"success":result.is_ok()});
                if let Err(error)=result{end["error"]=error.into();}
                event_sink(end);
                settled.abort(None);
                let mut logins=active_logins.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if logins.get(&provider).is_some_and(|current|current.signal().same_signal(&controller.signal())){logins.remove(&provider);}
            });
            return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"login_start".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Success{data:None}});
        },
        RpcCommandBody::LoginCancel{provider}=>{
            if let Some(active_logins)=active_logins && let Some(controller)=active_logins.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(provider){controller.abort(None);}
            ("login_cancel",Ok(None))
        },
        RpcCommandBody::LoginApiKey{provider,key}=>{
            let result=session.model_registry().auth_storage.set(provider,Some(serde_json::json!({"type":"api_key","key":key}))).map(|()|{let registry=session.model_registry();let mut runtime=registry.model_runtime.clone();tokio::spawn(async move{let _=runtime.refresh(Default::default()).await;});Some(serde_json::Value::Null)});
            ("login_api_key",result)
        },
        RpcCommandBody::Logout{provider}=>{
            let result=session.model_registry().auth_storage.logout(provider).map(|()|{let registry=session.model_registry();let mut runtime=registry.model_runtime.clone();tokio::spawn(async move{let _=runtime.refresh(Default::default()).await;});Some(serde_json::Value::Null)});
            ("logout",result)
        },
        RpcCommandBody::GetProviderAccounts{provider}=>{
            let result=session.model_registry().get_credential_accounts(provider,&session.agent_dir()).await
                .map(|accounts|Some(serde_json::json!({"accounts":accounts.into_iter().map(|account|serde_json::json!({
                    "name":account.name,"displayName":account.display_name,"source":account.source.as_str(),"blocked":account.blocked,"pinned":account.pinned,
                })).collect::<Vec<_>>() })));
            ("get_provider_accounts",result)
        },
        RpcCommandBody::AccountPin{provider,name}=>{
            let result=session.model_registry().pin_credential_account(provider,name.as_deref(),&session.agent_dir()).await.map(|()|None);
            ("account_pin",result)
        },
        RpcCommandBody::AccountRemove{provider,name}=>{
            let result=session.model_registry().remove_credential_account(provider,name,&session.agent_dir()).await.map(|()|None);
            ("account_remove",result)
        },
        RpcCommandBody::GetState=>("get_state",Ok(Some(build_rpc_session_state(session,None)))),
        RpcCommandBody::GetTree => {
            fn tree_node(node: maho_core::session_manager::SessionTreeNode) -> serde_json::Value {
                let mut value = serde_json::json!({"entry":node.entry,"children":node.children.into_iter().map(tree_node).collect::<Vec<_>>()});
                if let Some(label) = node.label { value["label"] = label.into(); }
                if let Some(timestamp) = node.label_timestamp { value["labelTimestamp"] = timestamp.into(); }
                value
            }
            let (tree, leaf_id) = session.with_session_manager(|manager| {
                let entries = manager.entries();
                let ids = entries.iter().filter_map(|entry| entry["id"].as_str()).collect::<std::collections::HashSet<_>>();
                let tree = entries.iter().filter(|entry| entry["parentId"].as_str().is_none_or(|parent| !ids.contains(parent)))
                    .filter_map(|entry| entry["id"].as_str().and_then(|id| manager.get_tree(Some(id))))
                    .map(tree_node).collect::<Vec<_>>();
                (tree, manager.leaf_id().map(str::to_owned))
            });
            ("get_tree", Ok(Some(serde_json::json!({"tree":tree,"leafId":leaf_id}))))
        },
        RpcCommandBody::ImportJsonl{input_path,cwd_override}=>("import_jsonl",session.import_from_jsonl(input_path,cwd_override.as_deref()).await.map(|cancelled|Some(serde_json::json!({"cancelled":!cancelled})))),
        RpcCommandBody::EditAssistantMessage{entry_id,text,expected_leaf_id,summarize,custom_instructions}=>{
            if entry_id.is_empty(){return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"edit_assistant_message".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Error{error:"edit_assistant_message requires a non-empty entryId and a text string".into(),error_code:None,error_data:None}});}
            let result=session.edit_assistant_message_coded(entry_id,text,maho_core::agent_session::TreeNavigationOptions{expected_leaf_id:expected_leaf_id.clone(),summarize:*summarize,custom_instructions:custom_instructions.clone(),..Default::default()}).await;
            let result=match result{Ok(result)=>Ok(result),Err(error)=>return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"edit_assistant_message".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Error{error:error.message,error_code:error.code.map(str::to_owned),error_data:None}})};
            ("edit_assistant_message",edit_response(session,result,"Edited assistant entry was not persisted"))
        },
        RpcCommandBody::EditUserMessage{entry_id,text,expected_leaf_id,summarize,custom_instructions}=>{
            if entry_id.is_empty(){return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"edit_user_message".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Error{error:"edit_user_message requires a non-empty entryId and a text string".into(),error_code:None,error_data:None}});}
            let result=session.edit_user_message_coded(entry_id,text,maho_core::agent_session::TreeNavigationOptions{expected_leaf_id:expected_leaf_id.clone(),summarize:*summarize,custom_instructions:custom_instructions.clone(),..Default::default()}).await;
            let result=match result{Ok(result)=>Ok(result),Err(error)=>return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"edit_user_message".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Error{error:error.message,error_code:error.code.map(str::to_owned),error_data:None}})};
            ("edit_user_message",edit_response(session,result,"Edited user entry was not persisted"))
        },
        RpcCommandBody::GetMedia{tool_call_id,content_index}=>{
            let index=if content_index.is_finite()&&content_index.fract()==0.0&&*content_index>=0.0{*content_index as usize}else{usize::MAX};
            let content=session.messages().into_iter().find_map(|message|{
                let result=message.as_tool_result()?;
                (result.tool_call_id==*tool_call_id).then_some(result.content.get(index)).flatten().and_then(|block|{
                    matches!(block,maho_ai::types::ContentBlock::Image(_)).then_some(serde_json::to_value(block).ok()).flatten()
                })
            });
            let result=content.map(|content|Some(serde_json::json!({"toolCallId":tool_call_id,"contentIndex":content_index,"content":content}))).ok_or_else(||crate::rpc_types::RPC_ERROR_MEDIA_NOT_FOUND.to_owned());
            ("get_media",result)
        },
        RpcCommandBody::GetLoadedSurfaces=>{
            let result = async {
            let (respond, receive) = tokio::sync::oneshot::channel();
            let request = maho_ext_api::mcp_wire::McpControlInventoryRequest { session_id: session.session_id(), respond: std::sync::Mutex::new(Some(respond)) };
            session.emit_native_extension_event(maho_ext_api::mcp_wire::MCP_CONTROL_INVENTORY_REQUEST_EVENT, &request).await;
            let claimed = request.respond.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take().is_none();
            let snapshot = if claimed { serde_json::from_value::<maho_ext_api::mcp_wire::McpWireStatusSnapshot>(receive.await.map_err(|error|format!("MCP inventory request failed: {error}"))?).map_err(|error|format!("MCP inventory response invalid: {error}"))? } else { session.mcp_wire_status_snapshot::<maho_ext_api::mcp_wire::McpWireStatusSnapshot>().unwrap_or_default() };
            let mcp_servers = snapshot.servers.iter().map(|server| {
                let status = server.status.as_ref().map(|status|serde_json::json!(status)).unwrap_or_else(||serde_json::json!(if server.server_info.is_some() { "connected" } else if server.auth_status == maho_ext_api::mcp_wire::McpWireAuthStatus::NotLoggedIn { "needs_auth" } else { "enabled" }));
                serde_json::json!({"name":server.name,"toolCount":server.tools.len(),"status":status,"authStatus":server.auth_status})
            }).collect::<Vec<_>>();
            let extensions=session.loaded_extension_surfaces().await;
            Ok(Some(serde_json::json!({"extensions":extensions,"mcpServers":mcp_servers})))
            }.await;
            ("get_loaded_surfaces",result)
        },
        RpcCommandBody::ExtensionRequest{name,data}=>{
            let name=name.trim();
            if name.is_empty(){("extension_request",Err("Extension RPC request name cannot be empty".into()))}
            else {("extension_request",session.extension_runner_request(name,data.clone().unwrap_or(serde_json::Value::Null)).await.map(Some))}
        },
        RpcCommandBody::SetClientInfo{..}=>return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"set_client_info".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Success{data:None}}),
        RpcCommandBody::GetProtocolInfo=>return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"get_protocol_info".into(),session_id:None,result:RpcResponseResult::Error{error:"get_protocol_info requires the RPC host runtime".into(),error_code:None,error_data:None}}),
        RpcCommandBody::OpenSession{..}=>return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"open_session".into(),session_id:None,result:RpcResponseResult::Error{error:"open_session requires the multi-session host runtime".into(),error_code:None,error_data:None}}),
        RpcCommandBody::CloseSession=>return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"close_session".into(),session_id:command.session_id.clone(),result:RpcResponseResult::Error{error:"close_session requires the multi-session host runtime".into(),error_code:None,error_data:None}}),
        RpcCommandBody::ListSessions{..}=>return Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:"list_sessions".into(),session_id:None,result:RpcResponseResult::Error{error:"list_sessions requires the multi-session host runtime".into(),error_code:None,error_data:None}}),
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
    active_logins:ActiveLogins,
    loaded_surfaces_subscription: std::sync::Mutex<Option<maho_ext_api::BusSubscription>>,
    loaded_surfaces_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}
impl RpcConnectionHandler{
    /// Build a handler over `session` and install the RPC extension UI on it.
    pub async fn create(session:AgentSession,out:std::sync::Arc<dyn Fn(serde_json::Value)+Send+Sync>,capabilities:Vec<String>)->Self{
        let pending:std::sync::Arc<std::sync::Mutex<crate::session_extension_ui_requests::SessionExtensionUiRequests>>=Default::default();
        let questions:std::sync::Arc<std::sync::Mutex<crate::connection_question_bridge::ConnectionQuestionBridge>>=Default::default();
        let ui=std::sync::Arc::new(RpcExtensionUi::new(out.clone(),pending.clone(),questions.clone(),capabilities));
        let shutdown_requested:std::sync::Arc<std::sync::atomic::AtomicBool>=Default::default();
        let sink=out.clone();
        let bindings=maho_core::agent_session::ExtensionBindings{ui_context:Some(ui.clone()),mode:Some(maho_ext_api::ExtensionMode::Rpc),on_error:Some(std::sync::Arc::new(move|error:&maho_ext_api::ExtensionError|{(sink)(serde_json::json!({"type":"extension_error","extensionPath":error.extension_path,"event":error.event,"error":error.error}));})),..Default::default()};
        // The CLI mount already starts host sessions; attach only replaces the connection context.
        if session.has_started_extension_lifecycle() {session.rebind_extension_context(bindings).await;}
        else {session.bind_extensions(bindings).await;}
        let inventory_session = session.weak_accessor();
        let inventory_out = out.clone();
        let (inventory_tx, mut inventory_rx) = tokio::sync::mpsc::unbounded_channel::<maho_ext_api::mcp_wire::McpWireStatusSnapshot>();
        let loaded_surfaces_subscription = session.bind_mcp_wire_status::<maho_ext_api::mcp_wire::McpWireStatusSnapshot>(std::sync::Arc::new(move |snapshot| {
            let _ = inventory_tx.send(snapshot.clone());
        })).await;
        let loaded_surfaces_task = tokio::spawn(async move {
            let mut fingerprint = None::<serde_json::Value>;
            while let Some(snapshot) = inventory_rx.recv().await {
                let Some(session) = inventory_session() else { break; };
                let extensions = session.loaded_extension_surfaces().await;
                let mcp_servers = snapshot.servers.iter().map(|server| {
                    let status = server.status.as_ref().map(|status|serde_json::json!(status)).unwrap_or_else(||serde_json::json!(if server.server_info.is_some() { "connected" } else if server.auth_status == maho_ext_api::mcp_wire::McpWireAuthStatus::NotLoggedIn { "needs_auth" } else { "enabled" }));
                    serde_json::json!({"name":server.name,"toolCount":server.tools.len(),"status":status,"authStatus":server.auth_status})
                }).collect::<Vec<_>>();
                let value = serde_json::json!({"extensions":extensions,"mcpServers":mcp_servers});
                let skills = session.loaded_skills().unwrap_or_default().skills.into_iter().map(|skill| serde_json::json!({"name":skill.name,"path":skill.file_path,"sourceInfo":skill.source_info,"enabled":!skill.disable_model_invocation})).collect::<Vec<_>>();
                let current = serde_json::json!({"skills":skills,"extensions":value["extensions"],"mcpServers":value["mcpServers"]});
                let changed = fingerprint.as_ref().is_some_and(|prior|prior != &current);
                fingerprint = Some(current);
                if changed { inventory_out(serde_json::json!({"type":"loaded_surfaces_changed"})); }
            }
        });
        Self{session,ui,pending,questions,shutdown_requested,active_logins:Default::default(),loaded_surfaces_subscription:std::sync::Mutex::new(loaded_surfaces_subscription),loaded_surfaces_task:tokio::sync::Mutex::new(Some(loaded_surfaces_task))}
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
        handle_input_line_with_login_state(&self.session,line,Some(self.ui.out.clone()),Some(self.active_logins.clone()),Some(self.ui.clone())).await
    }
    fn out_record(&self,record:serde_json::Value){(self.ui.out)(record);}
    pub fn is_shutdown_requested(&self)->bool{self.shutdown_requested.load(std::sync::atomic::Ordering::SeqCst)}
    /// Cancel UI requests that can no longer be answered by this connection.
    pub fn cancel_pending_extension_ui_requests(&self){self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).cancel_all();self.questions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).cancel_all(0);}
    /// Tear down subscriptions and dispose the session runtime. Never exits the process.
    pub async fn dispose(&self){self.loaded_surfaces_subscription.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();if let Some(task)=self.loaded_surfaces_task.lock().await.take(){task.abort();if let Err(error)=task.await && !error.is_cancelled(){eprintln!("RPC inventory consumer failed: {error}");}}self.cancel_pending_extension_ui_requests();self.session.emit_session_shutdown(maho_ext_api::SessionReason::Quit).await;self.session.dispose().await;}
}

#[cfg(test)]
mod new_session_tests{
    use super::*;
    use std::sync::Arc;

    fn session()->AgentSession{
        let runtime=maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions{providers:Some(vec![]),..Default::default()});
        AgentSession::new(maho_core::agent_session::AgentSessionConfig{
            agent:maho_agent::Agent::new(maho_agent::AgentOptions{stream_fn:Some(Arc::new(|_,_,_|maho_ai::types::AssistantMessageEventStream::assistant())),..Default::default()}),
            session_manager:maho_core::session_manager::SessionManager::in_memory("/tmp",None,None),
            settings_manager:maho_core::settings_manager::SettingsManager::from_storage(Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()),false),
            cwd:"/tmp".into(),agent_dir:Some("/tmp/maho-agent".into()),fallback_now:None,retry_random:None,scoped_models:vec![],favorite_models:vec![],flag_values:Default::default(),custom_tools:vec![],model_runtime:Some(runtime),model_registry:None,uses_default_stream_function:Some(false),initial_active_tool_names:None,default_tool_names:None,eval_only_tool_names:None,allowed_tool_names:None,excluded_tool_names:None,base_tools_override:None,session_start_event:None,auto_title_sessions:Some(false),
        }).expect("session")
    }

    #[tokio::test]
    async fn new_session_without_parent_returns_navigation_result(){
        let session=session();
        let command:RpcCommand=serde_json::from_value(serde_json::json!({"id":"new","type":"new_session"})).expect("command");
        let response=serde_json::to_value(handle_session_command(&session,&command).await.expect("response")).expect("wire");
        assert_eq!(response["command"],"new_session");
        assert_eq!(response["success"],true);
        assert_eq!(response["data"],serde_json::json!({"cancelled":false}));
    }

    #[tokio::test]
    async fn new_session_with_parent_preserves_success_shape(){
        let session=session();
        let command:RpcCommand=serde_json::from_value(serde_json::json!({"type":"new_session","parentSession":"parent-session"})).expect("command");
        let response=serde_json::to_value(handle_session_command(&session,&command).await.expect("response")).expect("wire");
        assert_eq!(response["command"],"new_session");
        assert_eq!(response["success"],true);
        assert_eq!(response["data"],serde_json::json!({"cancelled":false}));
    }

    #[tokio::test]
    async fn fork_preserves_response_shape_and_error_semantics(){
        let session=session();
        let command:RpcCommand=serde_json::from_value(serde_json::json!({"id":"fork","type":"fork","entryId":"missing"})).expect("command");
        let response=serde_json::to_value(handle_session_command(&session,&command).await.expect("response")).expect("wire");
        assert_eq!(response["command"],"fork");
        assert_eq!(response["success"],false);
        assert_eq!(response["error"],"Invalid entry ID for forking");
    }

    #[tokio::test]
    async fn clone_without_leaf_returns_exact_error(){
        let session=session();
        let command:RpcCommand=serde_json::from_value(serde_json::json!({"id":"clone","type":"clone"})).expect("command");
        let response=serde_json::to_value(handle_session_command(&session,&command).await.expect("response")).expect("wire");
        assert_eq!(response["command"],"clone");
        assert_eq!(response["success"],false);
        assert_eq!(response["error"],"Cannot clone session: no current entry selected");
    }

    #[tokio::test]
    async fn navigate_tree_requires_one_target(){
        let session=session();
        let command:RpcCommand=serde_json::from_value(serde_json::json!({"type":"navigate_tree"})).expect("command");
        let response=serde_json::to_value(handle_session_command(&session,&command).await.expect("response")).expect("wire");
        assert_eq!(response["error"],"navigate_tree requires entryId or targetId");
    }

    #[tokio::test]
    async fn get_tree_returns_tree_and_leaf_id(){
        let session=session();
        let command:RpcCommand=serde_json::from_value(serde_json::json!({"type":"get_tree"})).expect("command");
        let response=serde_json::to_value(handle_session_command(&session,&command).await.expect("response")).expect("wire");
        assert_eq!(response["command"],"get_tree");
        assert!(response["data"].get("tree").is_some());
        assert!(response["data"].get("leafId").is_some());
    }

    #[tokio::test]
    async fn import_jsonl_reports_missing_file(){
        let session=session();
        let command:RpcCommand=serde_json::from_value(serde_json::json!({"type":"import_jsonl","inputPath":"/tmp/does-not-exist-4010.jsonl"})).expect("command");
        let response=serde_json::to_value(handle_session_command(&session,&command).await.expect("response")).expect("wire");
        assert_eq!(response["command"],"import_jsonl");
        assert_eq!(response["error"],"File not found: /tmp/does-not-exist-4010.jsonl");
    }

    #[tokio::test]
    async fn append_user_message_dispatches(){
        let response=serde_json::to_value(handle_session_command(&session(),&serde_json::from_value(serde_json::json!({"type":"append_user_message","content":"hello"})).unwrap()).await.unwrap()).unwrap();
        assert_eq!(response["command"],"append_user_message"); assert_eq!(response["success"],true);
    }
    #[tokio::test]
    async fn get_media_reports_missing_media(){
        let response=serde_json::to_value(handle_session_command(&session(),&serde_json::from_value(serde_json::json!({"type":"get_media","toolCallId":"missing","contentIndex":0})).unwrap()).await.unwrap()).unwrap();
        assert_eq!(response["command"],"get_media"); assert_eq!(response["success"],false); assert_eq!(response["error"],"media_not_found");
    }
    #[tokio::test]
    async fn get_loaded_surfaces_dispatches_inventory(){
        let response=serde_json::to_value(handle_session_command(&session(),&serde_json::from_value(serde_json::json!({"type":"get_loaded_surfaces"})).unwrap()).await.unwrap()).unwrap();
        assert_eq!(response["command"],"get_loaded_surfaces"); assert_eq!(response["success"],true); assert!(response["data"]["extensions"].is_array()); assert!(response["data"]["mcpServers"].is_array());
    }
    #[tokio::test]
    async fn extension_request_rejects_empty_name(){
        let response=serde_json::to_value(handle_session_command(&session(),&serde_json::from_value(serde_json::json!({"type":"extension_request","name":"   "})).unwrap()).await.unwrap()).unwrap();
        assert_eq!(response["command"],"extension_request"); assert_eq!(response["success"],false); assert_eq!(response["error"],"Extension RPC request name cannot be empty");
    }
    #[tokio::test]
    async fn get_auth_providers_dispatches_provider_statuses(){
        let response=serde_json::to_value(handle_session_command(&session(),&serde_json::from_value(serde_json::json!({"type":"get_auth_providers"})).unwrap()).await.unwrap()).unwrap();
        assert_eq!(response["command"],"get_auth_providers"); assert_eq!(response["success"],true); assert!(response["data"]["providers"].is_array());
    }
    #[tokio::test]
    async fn login_api_key_persists_api_key_credential(){
        let session=session();
        let response=serde_json::to_value(handle_session_command(&session,&serde_json::from_value(serde_json::json!({"type":"login_api_key","provider":"fixture","key":"secret"})).unwrap()).await.unwrap()).unwrap();
        assert_eq!(response["command"],"login_api_key"); assert_eq!(response["success"],true); assert_eq!(session.model_registry().auth_storage.get("fixture"),Some(serde_json::json!({"type":"api_key","key":"secret"})));
    }
    #[tokio::test]
    async fn logout_removes_stored_credential(){
        let session=session(); session.model_registry().auth_storage.set("fixture",Some(serde_json::json!({"type":"api_key","key":"secret"}))).unwrap();
        let response=serde_json::to_value(handle_session_command(&session,&serde_json::from_value(serde_json::json!({"type":"logout","provider":"fixture"})).unwrap()).await.unwrap()).unwrap();
        assert_eq!(response["command"],"logout"); assert_eq!(response["success"],true); assert_eq!(session.model_registry().auth_storage.get("fixture"),None);
    }
    #[tokio::test]
    async fn bash_dispatches_success(){
        let response=serde_json::to_value(handle_session_command(&session(),&serde_json::from_value(serde_json::json!({"type":"bash","command":"printf rpc"})).unwrap()).await.unwrap()).unwrap();
        assert_eq!(response["command"],"bash"); assert_eq!(response["success"],true);
    }
    #[tokio::test]
    async fn abort_branch_summary_dispatches(){
        let response=serde_json::to_value(handle_session_command(&session(),&serde_json::from_value(serde_json::json!({"type":"abort_branch_summary"})).unwrap()).await.unwrap()).unwrap();
        assert_eq!(response["command"],"abort_branch_summary"); assert_eq!(response["success"],true);
    }
    #[tokio::test]
    async fn check_reload_veto_dispatches(){
        let response=serde_json::to_value(handle_session_command(&session(),&serde_json::from_value(serde_json::json!({"type":"check_reload_veto"})).unwrap()).await.unwrap()).unwrap();
        assert_eq!(response["command"],"check_reload_veto"); assert_eq!(response["success"],true); assert_eq!(response["data"],false);
    }
    #[tokio::test]
    async fn set_fast_mode_without_extension_returns_command_error(){
        let response=serde_json::to_value(handle_session_command(&session(),&serde_json::from_value(serde_json::json!({"type":"set_fast_mode","enabled":true})).unwrap()).await.unwrap()).unwrap();
        assert_eq!(response["command"],"set_fast_mode"); assert_eq!(response["success"],false); assert_eq!(response["error"],"Extension runner is not bound");
    }
    #[tokio::test]
    async fn send_custom_message_dispatches(){
        let response=serde_json::to_value(handle_session_command(&session(),&serde_json::from_value(serde_json::json!({"type":"send_custom_message","customType":"notice","content":[{"type":"text","text":"hello"}],"display":true})).unwrap()).await.unwrap()).unwrap();
        assert_eq!(response["command"],"send_custom_message"); assert_eq!(response["success"],true);
    }
    #[tokio::test]
    async fn edit_user_message_reports_exact_missing_entry_error(){
        let response=serde_json::to_value(handle_session_command(&session(),&serde_json::from_value(serde_json::json!({"type":"edit_user_message","entryId":"missing","text":"hello"})).unwrap()).await.unwrap()).unwrap();
        assert_eq!(response["success"],false); assert_eq!(response["error"],"Entry missing not found");
    }
    #[tokio::test]
    async fn edit_assistant_message_reports_exact_missing_entry_error(){
        let response=serde_json::to_value(handle_session_command(&session(),&serde_json::from_value(serde_json::json!({"type":"edit_assistant_message","entryId":"missing","text":"hello"})).unwrap()).await.unwrap()).unwrap();
        assert_eq!(response["success"],false); assert_eq!(response["error"],"Entry missing not found");
    }
    #[tokio::test]
    async fn get_provider_accounts_dispatches_empty_accounts(){
        let response=serde_json::to_value(handle_session_command(&session(),&serde_json::from_value(serde_json::json!({"type":"get_provider_accounts","provider":"openai"})).unwrap()).await.unwrap()).unwrap();
        assert_eq!(response["command"],"get_provider_accounts"); assert_eq!(response["success"],true); assert_eq!(response["data"],serde_json::json!({"accounts":[]}));
    }
    #[tokio::test]
    async fn account_pin_preserves_provider_error(){
        let response=serde_json::to_value(handle_session_command(&session(),&serde_json::from_value(serde_json::json!({"type":"account_pin","provider":"openai","name":"missing"})).unwrap()).await.unwrap()).unwrap();
        assert_eq!(response["command"],"account_pin"); assert_eq!(response["success"],false); assert_eq!(response["error"],"Provider account not found: missing");
    }
    #[tokio::test]
    async fn account_remove_preserves_provider_error(){
        let response=serde_json::to_value(handle_session_command(&session(),&serde_json::from_value(serde_json::json!({"type":"account_remove","provider":"openai","name":"missing"})).unwrap()).await.unwrap()).unwrap();
        assert_eq!(response["command"],"account_remove"); assert_eq!(response["success"],false); assert_eq!(response["error"],"Provider account not found: missing");
    }
}
