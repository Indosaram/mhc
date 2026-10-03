use std::sync::Arc;

use maho_cli::cli::tool_search::{mcp_tool_search_argument, SharedToolSearch};
use maho_ext_api::{
    CustomMessage, ExtensionActions, ExtensionFailure, ExtensionRuntime, SendMessageOptions,
    SendUserMessageOptions, ToolInfo, UserMessageContent,
};

struct NoActions;

impl ExtensionActions for NoActions {
    fn send_message(&self, _message: CustomMessage, _options: SendMessageOptions) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn send_user_message(&self, _content: UserMessageContent, _options: SendUserMessageOptions) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn append_entry(&self, _custom_type: &str, _data: Option<serde_json::Value>) -> Result<(), ExtensionFailure> {
        Ok(())
    }
    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> {
        Ok(Vec::new())
    }
}

#[test]
fn mcp_receives_the_canonical_shared_service() {
    let shared = SharedToolSearch::new(ExtensionRuntime::default(), Arc::new(NoActions));
    let handle = mcp_tool_search_argument(&shared);
    assert!(Arc::ptr_eq(&handle, shared.service()));
    let moved = SharedToolSearch::new(ExtensionRuntime::default(), Arc::new(NoActions));
    let inner = moved.service().clone();
    assert!(Arc::ptr_eq(&moved.into_mcp_argument(), &inner));
}
