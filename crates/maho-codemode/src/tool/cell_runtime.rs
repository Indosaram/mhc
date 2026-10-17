use std::sync::Arc;
use serde_json::{Value, json};
use maho_ext_api::{AgentToolResult, ContentBlock, ImageContent};
use super::{types::{EvalToolInput, EvalRuntimeInfo}, call_capture::EvalToolCallMetric, image::{EvalOutputCollector, EvalOutputOptions, EvalOutputResult}};

pub struct CellState {
    pub input: EvalToolInput,
    pub runtime: Option<EvalRuntimeInfo>,
    pub started_at: f64,
    pub run_started_at: Option<f64>,
    pub queued_behind: Option<Vec<String>>,
    pub on_update: Option<Arc<dyn Fn(AgentToolResult) + Send + Sync>>,
    pub tool_calls: Vec<Value>,
    pub tool_call_metrics: Vec<EvalToolCallMetric>,
    pub status_events: Vec<Value>,
    pub active: bool,
    pub output: String,
    pub phase: Option<String>,
    pub error: Option<String>,
    pub duration_ms: f64,
    pub status: String,
}

// `state.output` holds the finished output only; while running, `details` reads the collector's
// aggregate directly instead of copying it into `state.output` on every frame.
pub struct CellResultBuilder { pub state: CellState, output: EvalOutputCollector, finished: bool, coalesce_updates: bool, update_pending: bool }

impl CellResultBuilder {
    pub fn new(mut state: CellState, options: EvalOutputOptions) -> Self {
        if state.status != "queued" { state.status = "running".into(); }
        let mut builder = Self {state,output:EvalOutputCollector::new(options),finished:false,coalesce_updates:false,update_pending:false};
        builder.emit_update(false);
        builder
    }

    /// Frame-driven updates (output, phase, status) only mark an update pending; the run loop
    /// publishes it through [`Self::flush_update`] once a burst of kernel frames drains.
    pub fn coalesce_updates(&mut self) {self.coalesce_updates=true;}
    pub fn request_update(&mut self) {if self.coalesce_updates {self.update_pending=true;} else {self.emit_update(false);}}
    pub fn flush_update(&mut self) {if self.update_pending {self.emit_update(false);}}
    pub fn update_pending(&self) -> bool {self.update_pending}

    pub async fn push(&mut self, text: &str) -> Result<(), String> {
        self.output.push(text).await?;
        self.request_update();
        Ok(())
    }
    pub async fn display(&mut self, mime_type: &str, data_base64: &str) -> Result<(), String> {
        self.output.display(mime_type,data_base64).await?;
        self.request_update();
        Ok(())
    }
    pub fn set_phase(&mut self, title: String) {self.state.phase=Some(title);self.request_update();}
    pub async fn flush_output(&mut self) -> Result<(), String> {self.output.flush().await}

    pub async fn finalize(&mut self, result: &Value) -> Result<AgentToolResult, String> {
        self.state.duration_ms=result["durationMs"].as_f64().unwrap_or(0.0);
        let ok=result["ok"]==true;
        if ok {
            if let Some(value)=result["valueRepr"].as_str().filter(|value|!value.is_empty()) {self.push(&format!("{value}\n")).await?;}
            self.state.status="complete".into();
        } else {
            let error=result["error"]["message"].as_str().unwrap_or("").to_owned();
            self.state.error=Some(error.clone());
            self.push(&format!("{error}\n")).await?;
            self.state.status="error".into();
        }
        self.finish(!ok).await
    }

    pub async fn finalize_cancellation(&mut self, error: &str) -> Result<AgentToolResult, String> {
        self.state.error=Some(error.into());
        self.push(&format!("{error}\n")).await?;
        self.state.status="error".into();
        self.finish(true).await
    }

    async fn finish(&mut self, is_error: bool) -> Result<AgentToolResult, String> {
        let output=self.output.finish().await?;
        self.state.output=output.output.clone();
        self.finished=true;
        let details=self.details(Some(&output),is_error);
        self.emit_update(is_error);
        let text=if !output.output.is_empty() {output.output.clone()} else if !output.images.is_empty() {format!("(displayed {} image{}; no text output)",output.images.len(),if output.images.len()==1 {""} else {"s"})} else {"(no output)".into()};
        let mut result=AgentToolResult::text(text);
        result.content.extend(output.images.into_iter().map(|image|ContentBlock::Image(ImageContent {data:image.data,mime_type:image.mime_type})));
        result.details=details;
        Ok(result)
    }

    pub fn live_result(&self) -> AgentToolResult {
        let mut result=AgentToolResult::text(self.live_update_text());
        result.details=self.details(None,self.state.status=="error");
        result
    }

    pub fn emit_update(&mut self, is_error: bool) {
        self.update_pending=false;
        if self.state.active && let Some(update)=&self.state.on_update {
            let mut result=AgentToolResult::text(self.live_update_text());
            result.details=self.details(None,is_error);
            update(result);
        }
    }

    fn details(&self, output: Option<&EvalOutputResult>, is_error: bool) -> Value {
        let state=&self.state;
        let cell_output=if self.finished {state.output.as_str()} else {self.output.aggregate_text()};
        let mut cell=json!({"index":0,"summary":state.input.summary,"code":state.input.code,"language":state.input.language,"output":cell_output,"status":state.status,"durationMs":state.duration_ms});
        if state.status!="queued" {cell["startedAt"]=json!(state.run_started_at.unwrap_or(state.started_at));}
        if let Some(runtime)=&state.runtime {cell["runtime"]=json!(runtime);}
        if let Some(queued)=&state.queued_behind {cell["queuedBehind"]=json!(queued);}
        if !state.status_events.is_empty() {cell["statusEvents"]=json!(state.status_events);}
        if output.is_some_and(|output|output.has_markdown) {cell["hasMarkdown"]=json!(true);}
        let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("system clock").as_secs_f64()*1000.0;
        let mut details=json!({"language":state.input.language,"languages":[state.input.language],"summary":state.input.summary,"durationMs":state.duration_ms,"wallDurationMs":(now-state.started_at).max(0.0),"toolCallCount":state.tool_call_metrics.len(),"toolCalls":state.tool_calls,"truncated":output.is_some_and(|output|output.truncated),"cells":[cell]});
        if let Some(runtime)=&state.runtime {details["runtime"]=json!(runtime);}
        if is_error {details["isError"]=json!(true);}
        if let Some(phase)=&state.phase {details["phase"]=json!(phase);}
        if !state.status_events.is_empty() {details["statusEvents"]=json!(state.status_events);}
        if let Some(output)=output {
            if !output.json_outputs.is_empty() {details["jsonOutputs"]=json!(output.json_outputs);}
            if let Some(notice)=&output.notice {details["notice"]=json!(notice);}
            if let Some(meta)=&output.meta {details["meta"]=meta.clone();}
        }
        details
    }

    fn live_update_text(&self) -> String {
        let state=&self.state;
        if state.status=="queued" && let Some(queued)=state.queued_behind.as_ref().filter(|queued|!queued.is_empty()) {
            let language=serde_json::to_value(state.input.language).expect("language value");
            return format!("queued behind {} in the {} kernel",queued.join(", "),language.as_str().expect("language string"));
        }
        let aggregate=self.output.aggregate_text();
        let trailing=aggregate.ends_with('\n');
        let body=if trailing {&aggregate[..aggregate.len()-1]} else {aggregate};
        // Only the last 8 lines are shown; find them from the end instead of splitting it all.
        let start=body.rmatch_indices('\n').nth(7).map_or(0,|(index,_)|index+1);
        let output=format!("{}{}",&body[start..],if trailing {"\n"} else {""});
        let language=serde_json::to_value(state.input.language).expect("language value");
        format!("1/1 cells {}\n[1] {} {} {}{}",state.status,language.as_str().expect("language string"),state.input.summary,state.status,if output.is_empty() {String::new()} else {format!("\n{output}")})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{image_resize::*, types::{EvalLanguage, EvalToolInput}};
    struct Sdk;
    impl EvalImageSdk for Sdk {
        fn resize_image<'a>(&'a self, _:Vec<u8>, _: &'a str, _:Option<usize>) -> ImageFuture<'a, Option<ResizedImage>> {Box::pin(async {Err("unexpected image resize".into())})}
        fn convert_to_png<'a>(&'a self, _: &'a str, _: &'a str) -> ImageFuture<'a, Option<EvalImageContent>> {Box::pin(async {Err("unexpected conversion".into())})}
    }
    #[tokio::test]
    async fn coalesced_frames_publish_on_flush_and_finish_always_emits() {
        let updates=Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink=updates.clone();
        let state=CellState {input:EvalToolInput {language:EvalLanguage::Js,code:"print(1)".into(),summary:"burst".into(),action:None,timeout:None,on_timeout:None,reset:None},runtime:None,started_at:0.0,run_started_at:Some(0.0),queued_behind:None,on_update:Some(Arc::new(move |result:AgentToolResult|sink.lock().unwrap().push(result.details["cells"][0]["output"].as_str().unwrap_or("").to_owned()))),tool_calls:vec![],tool_call_metrics:vec![],status_events:vec![],active:true,output:String::new(),phase:None,error:None,duration_ms:0.0,status:"running".into()};
        let mut builder=CellResultBuilder::new(state,EvalOutputOptions {artifact_path:None,head_bytes:20480,max_columns:768,provider:None,api:None,image_sdk:Arc::new(Sdk)});
        builder.coalesce_updates();
        builder.push("a\n").await.unwrap();builder.push("b\n").await.unwrap();builder.set_phase("p".into());
        assert_eq!(updates.lock().unwrap().len(),1);
        assert!(builder.update_pending());
        builder.flush_update();assert!(!builder.update_pending());builder.flush_update();
        assert_eq!(updates.lock().unwrap().as_slice(),["","a\nb\n"]);
        builder.push("c\n").await.unwrap();
        builder.finalize(&serde_json::json!({"type":"result","ok":true,"durationMs":1})).await.unwrap();
        assert_eq!(updates.lock().unwrap().last().map(String::as_str),Some("a\nb\nc"));
        assert_eq!(updates.lock().unwrap().len(),3);
    }
}
