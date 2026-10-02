#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
