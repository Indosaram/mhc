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
pub async fn handle_input_line(session:&AgentSession,line:&str)->Result<Option<String>,serde_json::Error>{
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
    handle_session_command(session,&command).await.map(|response|crate::jsonl::serialize_json_line(&response)).transpose()
}
pub async fn handle_session_command(session:&AgentSession,command:&RpcCommand)->Option<RpcResponse>{
    let (kind,result)=match &command.body{
        RpcCommandBody::Abort=>{
            let owned=session.clone();
            let mut abort=Box::pin(async move{owned.abort().await;});
            let completed=std::future::poll_fn(|context|std::task::Poll::Ready(abort.as_mut().poll(context).is_ready())).await;
            if !completed{tokio::spawn(abort);}
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
        _=>return None,
    };
    Some(RpcResponse{id:command.id.clone(),record_type:ResponseRecordType::Response,command:kind.into(),session_id:command.session_id.clone(),result:match result{Ok(data)=>RpcResponseResult::Success{data},Err(error)=>RpcResponseResult::Error{error,error_code:None,error_data:None}}})
}
