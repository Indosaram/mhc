use super::types::EvalLanguage;

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct EvalKernelResetRefusedError {
    pub name: &'static str,
    pub code: &'static str,
    pub message: String,
}

impl EvalKernelResetRefusedError {
    pub fn new(language: EvalLanguage, live_cell_ids: &[String]) -> Self {
        let language = match language { EvalLanguage::Js => "js", EvalLanguage::Py => "py", EvalLanguage::Rb => "rb", EvalLanguage::Jl => "jl" };
        Self {
            name: "EvalKernelResetRefusedError",
            code: "eval_kernel_busy_reset_refused",
            message: format!("eval_kernel_busy_reset_refused: Cannot reset the {language} kernel: live cells {}. Stop them with eval({{ action: \"stop\", cell_id }}) or wait for their notifications, then reset.", live_cell_ids.join(", ")),
        }
    }
}
