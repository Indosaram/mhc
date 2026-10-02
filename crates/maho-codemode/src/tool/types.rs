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
