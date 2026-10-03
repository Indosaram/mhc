use std::sync::Arc;
use serde_json::{Value, json};
use maho_ext_api::ExecuteToolOptions;
use crate::{bridges::{agent_bridge::AgentBridge, output_bridge::OutputExecuteTool, schema_bridge::EvalToolCatalog, reserved_dispatch::{ReservedDispatchContext, is_reserved_tool_name, run_reserved_tool}, schema_hint::append_schema_hint}, config::settings::CodemodeSettings};
use super::{cell_runtime::CellResultBuilder, call_capture::*, tool_result_marshal::{marshal_tool_result, tool_result_is_error}, status_events::upsert_status_event};

#[derive(Clone)]
pub struct CellBridgeRuntime {
    pub executor: Arc<dyn OutputExecuteTool>,
    pub tools: Option<EvalToolCatalog>,
    pub settings: CodemodeSettings,
    pub signal: maho_ai::utils::abort::AbortSignal,
    pub deliver_reply: Arc<dyn Fn(Value) + Send + Sync>,
    pub complete: Option<CellCompletionHandler>,
}


pub type CellCompletionHandler = Arc<dyn Fn(crate::completion::handler::CompletionRequest, maho_ai::utils::abort::AbortSignal, Option<super::eval_tool_options::EvalInvocationContext>) -> std::pin::Pin<Box<dyn std::future::Future<Output=Result<Value,crate::completion::handler::CompletionError>> + Send>> + Send + Sync>;

pub struct CellHandler {
    pub builder: CellResultBuilder,
    runtime: CellBridgeRuntime,
    agent_bridge: Arc<AgentBridge>,
    emit_status: Option<crate::bridges::agent_bridge::AgentStatusEmitter>,
}

pub struct PendingCellToolResult {
    message: Value,
    capture: ToolCallCapture,
    metric_index: usize,
    error_code: Option<&'static str>,
    reply: Result<(Value,bool,Option<String>,Option<String>),String>,
}
pub type PendingCellToolCall = std::pin::Pin<Box<dyn std::future::Future<Output=PendingCellToolResult> + Send>>;

impl CellHandler {
    pub fn new(builder: CellResultBuilder, runtime: CellBridgeRuntime) -> Self { let agent_bridge=AgentBridge::for_executor(&runtime.executor);Self {builder,runtime,agent_bridge,emit_status:None} }
    pub fn set_status_emitter(&mut self, emitter:crate::bridges::agent_bridge::AgentStatusEmitter) {self.emit_status=Some(emitter);}

    pub async fn handle(&mut self, message: &Value) -> Result<(), String> {
        if !self.builder.state.active { return Ok(()); }
        match message["type"].as_str() {
            Some("text") => self.builder.push(message["data"].as_str().unwrap_or("")).await?,
            Some("phase") => self.builder.set_phase(message["title"].as_str().unwrap_or("").into()),
            Some("status") => if self.runtime.settings.status_events { upsert_status_event(&mut self.builder.state.status_events,message["event"].clone());self.builder.emit_update(false); },
            Some("log") => self.builder.push(&format!("{}\n",message["message"].as_str().unwrap_or(""))).await?,
            Some("display") => self.builder.display(message["mimeType"].as_str().unwrap_or(""),message["dataBase64"].as_str().unwrap_or("")).await?,
            Some("tool-call") => self.handle_tool_call(message).await?,
            Some("ready" | "init-failed" | "result" | "closed" | "kernel-tool-describe-reply" | "kernel-tool-invoke-reply") => {},
            _ => return Err(format!("Unhandled kernel message: {message}")),
        }
        Ok(())
    }

    async fn handle_tool_call(&mut self, message: &Value) -> Result<(), String> {
        let pending=self.begin_tool_call(message);
        self.finish_tool_call(pending.await)
    }

    pub fn begin_tool_call(&mut self, message: &Value) -> PendingCellToolCall {
        let started=now_ms();
        let name=message["toolName"].as_str().unwrap_or("");
        let call_id=message["callId"].as_str().unwrap_or("");
        let metric=create_tool_call_metric(name,started);
        let metric_index=self.builder.state.tool_call_metrics.len();
        self.builder.state.tool_call_metrics.push(metric.clone());
        let args=bound_tool_call_args(&message["args"]);
        let capture=ToolCallCapture {call_id:cap_code_points(call_id,MAX_CAPTURED_IDENTIFIER_CODE_POINTS),args:args.args,started_at:started,metric,include_details:name!=crate::bridge::reserved::RESERVED_SCHEMA_TOOL,args_truncated:args.truncated};
        let runtime=self.runtime.clone();
        let agent_bridge=self.agent_bridge.clone();
        let emit_status=self.emit_status.clone();
        let message=message.clone();
        Box::pin(async move {
        let name=message["toolName"].as_str().unwrap_or("");
        let call_id=message["callId"].as_str().unwrap_or("");
        let options=ExecuteToolOptions {signal:Some(runtime.signal.clone()),..Default::default()};
        let mut error_code=None;
        let reply = if name=="completion" && let Some(complete)=runtime.complete.as_ref() {
            match crate::completion::tool_bridge::to_completion_request(&message["args"]) {
                Ok(request)=>complete(request,runtime.signal.clone(),None).await.map(|result| (result.get("value").or_else(||result.get("text")).cloned().unwrap_or(Value::Null),true,None,None)).map_err(|error|error.0),
                Err(error)=>Err(error.0),
            }
        } else if name=="eval" {
            Err("recursive eval is not allowed".into())
        } else if is_reserved_tool_name(name) {
            let tools=if name==crate::bridge::reserved::RESERVED_OUTPUT_TOOL {Ok(None)} else {runtime.tools.as_ref().map(|list|list()).transpose()};
            match tools {
                Ok(tools)=>run_reserved_tool(name,ReservedDispatchContext {call_id,args:&message["args"],executor:runtime.executor.as_ref(),task_tool_name:&runtime.settings.task_tools.task,task_output_tool_name:&runtime.settings.task_tools.output,tools:tools.as_deref(),execute_options:options,emit_status,agent_bridge:&agent_bridge}).await.map(|value|(value,true,None,None)).map_err(|error| {error_code=error.code();error.to_string()}),
                Err(error)=>Err(error),
            }
        } else {
            runtime.executor.execute_tool(name,message["args"].clone(),options).await.map(|result| {
                let ok=!tool_result_is_error(&result);
                let preview=ok.then(||tool_call_result_preview(&result)).flatten();
                let error=(!ok).then(||result.content.iter().find_map(|part| match part {maho_ext_api::ContentBlock::Text(content)=>Some(cap_code_points(&crate::host_sdk::sanitize_terminal_label(&content.text),512)),_=>None})).flatten();
                (marshal_tool_result(&result),ok,preview,error)
            }).map_err(|error| {
                error_code=Some(crate::bridges::reserved_dispatch::execute_tool_error_code(&error.code));
                error.to_string()
            })
        };
        PendingCellToolResult {message,capture,metric_index,error_code,reply}
        })
    }

    pub fn finish_tool_call(&mut self, result: PendingCellToolResult) -> Result<(), String> {
        let PendingCellToolResult {message,mut capture,metric_index,error_code,reply}=result;
        let name=message["toolName"].as_str().unwrap_or("");
        let call_id=message["callId"].as_str().unwrap_or("");
        if !self.builder.state.active {return Ok(());}
        match reply {
            Ok((value,ok,preview,error)) => {
                record_tool_call(&mut self.builder.state.tool_calls,ok,&mut capture,preview.as_deref(),error.as_deref(),now_ms());
                (self.runtime.deliver_reply)(json!({"type":"tool-reply","callId":call_id,"ok":true,"value":value}));
            }
            Err(error) => {
                let tools=if name=="eval" || name=="completion" {None} else {self.runtime.tools.as_ref().map(|list|list()).transpose()?};
                let parameters=tools.as_ref().and_then(|tools|tools.iter().find(|tool|tool.name==name)).and_then(|tool|tool.parameters.as_ref());
                let error=if name=="eval" {error} else {parameters.map_or_else(||error.clone(),|schema|append_schema_hint(&error,name,schema))};
                record_tool_call(&mut self.builder.state.tool_calls,false,&mut capture,None,Some(&error),now_ms());
                let mut payload=json!({"message":error});
                if let Some(code)=error_code {payload["code"]=json!(code);}
                (self.runtime.deliver_reply)(json!({"type":"tool-reply","callId":call_id,"ok":false,"error":payload}));
            }
        }
        self.builder.state.tool_call_metrics[metric_index]=capture.metric;
        self.builder.emit_update(false);
        Ok(())
    }
}

fn now_ms() -> f64 {std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("system clock").as_secs_f64()*1000.0}
