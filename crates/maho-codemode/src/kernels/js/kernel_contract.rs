use std::sync::Arc;

pub enum KernelToolNameSource {
    Names(Vec<String>),
    Provider(Arc<dyn Fn() -> Vec<String> + Send + Sync>),
}

pub fn resolve_kernel_tool_name_source(names: Option<&KernelToolNameSource>) -> Vec<String> {
    match names {
        Some(KernelToolNameSource::Names(names)) => names.clone(),
        Some(KernelToolNameSource::Provider(provider)) => provider(),
        None => Vec::new(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelOperation { Run, Reset, Interrupt }

impl std::fmt::Display for KernelOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self { Self::Run => "run", Self::Reset => "reset", Self::Interrupt => "interrupt" })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleState { Open, Closing, Closed }

#[derive(Debug, thiserror::Error)]
#[error("Cannot {operation}: JavaScript kernel is closed")]
pub struct JavaScriptKernelClosedError { pub operation: KernelOperation }

pub fn assert_javascript_kernel_open(lifecycle: LifecycleState, operation: KernelOperation) -> Result<(), JavaScriptKernelClosedError> {
    if lifecycle == LifecycleState::Open { Ok(()) } else { Err(JavaScriptKernelClosedError { operation }) }
}
