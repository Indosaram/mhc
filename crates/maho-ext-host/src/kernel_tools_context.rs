use std::{future::Future, sync::Arc};
use maho_ext_api::ExtensionKernelTools;

tokio::task_local! {
    static KERNEL_TOOLS: Arc<dyn ExtensionKernelTools>;
}

pub async fn with_kernel_tools<T>(tools: Arc<dyn ExtensionKernelTools>, invocation: impl Future<Output = T>) -> T {
    KERNEL_TOOLS.scope(tools, invocation).await
}

pub fn current_kernel_tools() -> Option<Arc<dyn ExtensionKernelTools>> {
    KERNEL_TOOLS.try_with(Arc::clone).ok()
}
