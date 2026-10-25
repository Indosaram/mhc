//! `rpc/start-cleanup.ts`: always dispose after terminating an unstarted child.
use super::handle::RpcChildHandle;
use crate::runners::types::TerminateOptions;
use std::sync::Arc;
pub fn discard_unstarted_rpc_handle(handle: Arc<RpcChildHandle>) -> std::io::Result<()> {
    let result = handle.terminate_with(TerminateOptions::default());
    handle.dispose_handle();
    result
}
