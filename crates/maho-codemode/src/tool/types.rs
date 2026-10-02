#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EvalLanguage { Js, Py, Rb, Jl }

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EvalRuntimeInfo {
    pub name: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

pub const EVAL_LANGUAGE_ORDER: [EvalLanguage; 4] = [EvalLanguage::Js, EvalLanguage::Py, EvalLanguage::Rb, EvalLanguage::Jl];

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimeoutBehavior { Detach, Error }

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EvalToolInput {
    pub language: EvalLanguage,
    pub code: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_timeout: Option<TimeoutBehavior>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset: Option<bool>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EvalToolRequest {
    Run(EvalToolInput),
    List,
    Peek { cell_id: String },
    Stop { cell_id: String },
}

pub type EvalKernelFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, String>> + Send + 'a>>;
pub struct EvalKernelRunInput {
    pub cell_id: String,
    pub code: String,
    pub timeout_ms: Option<u64>,
    pub on_started: Option<crate::kernels::shared::subprocess_run::KernelStartedCallback>,
    pub on_message: Option<crate::kernels::shared::subprocess_run::KernelMessageCallback>,
}
pub struct KernelInterruptHandle {
    pub state_retained: EvalKernelFuture<'static, bool>,
    pub note: Option<String>,
}
pub trait EvalKernel: Send + Sync {
    fn run(&self, input: EvalKernelRunInput) -> EvalKernelFuture<'_, serde_json::Value>;
    fn cancel_queued<'a>(&'a self, cell_id: &'a str, reason: &'a str) -> EvalKernelFuture<'a, bool>;
    fn interrupt<'a>(&'a self, reason: &'a str, cell_id: Option<&'a str>) -> EvalKernelFuture<'a, KernelInterruptHandle>;
    fn queue_snapshot(&self) -> (Option<String>, Vec<String>);
    fn deliver_tool_reply(&self, message: serde_json::Value) -> Result<(), String>;
    fn reset(&self) -> EvalKernelFuture<'_, ()>;
    fn close(&self) -> EvalKernelFuture<'_, ()>;
}

impl EvalKernel for crate::kernels::py::kernel::PythonKernel {
    fn run(&self, input: EvalKernelRunInput) -> EvalKernelFuture<'_, serde_json::Value> {
        Box::pin(self.run(crate::kernels::py::kernel_contract::PythonKernelRunOptions {cell_id:input.cell_id,code:input.code,timeout_ms:input.timeout_ms,on_started:input.on_started,on_message:input.on_message}))
    }
    fn cancel_queued<'a>(&'a self, cell_id: &'a str, reason: &'a str) -> EvalKernelFuture<'a, bool> {Box::pin(async move {Ok(self.cancel_queued(cell_id,reason).await)})}
    fn interrupt<'a>(&'a self, reason: &'a str, cell_id: Option<&'a str>) -> EvalKernelFuture<'a, KernelInterruptHandle> {
        Box::pin(async move {
            let retained=self.interrupt(reason,cell_id).await?;
            Ok(KernelInterruptHandle {state_retained:Box::pin(async move {Ok(retained)}),note:None})
        })
    }
    fn queue_snapshot(&self) -> (Option<String>,Vec<String>) {
        self.queue_snapshot()
    }
    fn deliver_tool_reply(&self, _:serde_json::Value) -> Result<(),String> {Ok(())}
    fn reset(&self) -> EvalKernelFuture<'_,()> {Box::pin(self.reset())}
    fn close(&self) -> EvalKernelFuture<'_,()> {Box::pin(self.close())}
}

impl EvalKernel for crate::kernels::shared::subprocess_kernel::SubprocessKernel {
    fn run(&self, input: EvalKernelRunInput) -> EvalKernelFuture<'_, serde_json::Value> {
        Box::pin(async move { self.run_with_callbacks(crate::kernels::shared::subprocess_contract::KernelRunInput {cell_id:input.cell_id,code:input.code,timeout_ms:input.timeout_ms},input.on_message,input.on_started).await.map_err(|error|error.to_string()) })
    }
    fn cancel_queued<'a>(&'a self, cell_id:&'a str, reason:&'a str)->EvalKernelFuture<'a,bool> {Box::pin(async move {Ok(self.cancel_queued(cell_id,reason).await)})}
    fn interrupt<'a>(&'a self, reason:&'a str, cell_id:Option<&'a str>)->EvalKernelFuture<'a,KernelInterruptHandle> {
        Box::pin(async move {let retained=self.interrupt(reason,cell_id).await?;Ok(KernelInterruptHandle {state_retained:Box::pin(async move {Ok(retained)}),note:None})})
    }
    fn queue_snapshot(&self)->(Option<String>,Vec<String>) {self.queue_snapshot()}
    fn deliver_tool_reply(&self,message:serde_json::Value)->Result<(),String> {self.deliver_tool_reply(message)}
    fn reset(&self)->EvalKernelFuture<'_,()> {Box::pin(async move {self.reset().await.map_err(|error|error.to_string())})}
    fn close(&self)->EvalKernelFuture<'_,()> {Box::pin(async move {self.close().await.map_err(|error|error.to_string())})}
}

pub struct EvalDeadlineSeconds {
    pub run_budget_seconds: f64,
    pub detach_after_seconds: f64,
    pub foreground_window_seconds: f64,
    pub hard_limit_seconds: f64,
}

impl Default for EvalDeadlineSeconds {
    fn default() -> Self {
        use crate::config::settings::*;
        Self { run_budget_seconds:DEFAULT_RUN_BUDGET_SECONDS,detach_after_seconds:CodemodeSettings::default().cell_timeout_seconds.min(DEFAULT_FOREGROUND_WINDOW_SECONDS),foreground_window_seconds:DEFAULT_FOREGROUND_WINDOW_SECONDS,hard_limit_seconds:DEFAULT_HARD_LIMIT_SECONDS }
    }
}

pub fn create_eval_input_schema(enabled: &crate::config::settings::Languages, deadlines: &EvalDeadlineSeconds) -> Result<serde_json::Value, &'static str> {
    use serde_json::json;
    let languages:Vec<_>=EVAL_LANGUAGE_ORDER.iter().filter(|language|match language {EvalLanguage::Js=>enabled.js,EvalLanguage::Py=>enabled.py,EvalLanguage::Rb=>enabled.rb,EvalLanguage::Jl=>enabled.jl}).map(|language|json!({"const":language,"type":"string"})).collect();
    if languages.is_empty() {return Err("eval requires at least one enabled language");}
    let timeout_description=format!("Run budget in seconds for this cell's own execution (default {}s); time parked in host tool calls such as agent() or tool.* is not charged. When it runs out the cell is killed, and a js cell that cannot settle (a pending timer or Bun.$ command, a synchronous call) restarts its kernel and loses every global. Raise it only for a declared long run; a value above {}s also raises the wall-clock hard limit. It does not move the detach point.",deadlines.run_budget_seconds,deadlines.hard_limit_seconds);
    let on_timeout_description=format!("'detach' (interactive default): the call returns after {}s of the cell's own work (a host tool call in flight can hold it up to the {}s foreground window) while the cell keeps running; completion arrives as a notification. 'error' (print/json default): the call blocks until the cell settles or a deadline kills it.",deadlines.detach_after_seconds,deadlines.foreground_window_seconds);
    Ok(json!({"type":"object","properties":{
        "action":{"anyOf":[{"const":"run","type":"string"},{"const":"peek","type":"string"},{"const":"stop","type":"string"},{"const":"list","type":"string"}],"description":"Defaults to run. peek and stop require cell_id. list: live and recently settled cells across languages."},
        "language":{"anyOf":languages},"code":{"type":"string","description":"Cell body, verbatim."},
        "summary":{"type":"string","description":"REQUIRED for run. One line in the language the user writes in: a progress update saying what you are doing and why, not a label for the code; shown in the TUI while the cell runs."},
        "timeout":{"type":"number","minimum":1,"description":timeout_description},
        "on_timeout":{"anyOf":[{"const":"detach","type":"string"},{"const":"error","type":"string"}],"description":on_timeout_description},
        "reset":{"type":"boolean","description":"Reset this language kernel before running; refused while that language has live cells."},
        "cell_id":{"type":"string","minLength":1,"description":"Eval cell id for peek or stop."}
    },"anyOf":[{"properties":{"action":{"enum":["run","list"]}}},{"properties":{"action":{"enum":["peek","stop"]}},"required":["action","cell_id"]}]}))
}
