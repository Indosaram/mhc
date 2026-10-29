//! Port of `team/member-extension/workpool-startup.test.ts`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::team::member_extension::index::{
    ExtensionApi, ExtensionEventHandler, MemberExtensionOptions, register_member_extension,
};
use crate::team::member_extension::tools::MemberTaskSendTool;
use crate::team::member_extension::index::{CustomMessage, SendMessageOptions};

#[derive(Default)]
struct NoopApi;

impl ExtensionApi for NoopApi {
    fn send_message(&self, _message: CustomMessage, _options: SendMessageOptions) {}
    fn register_tool(&self, _tool: MemberTaskSendTool) { panic!("workpool graph must not evaluate"); }
    fn on(&self, _event: &str, _handler: ExtensionEventHandler) { panic!("workpool graph must not evaluate"); }
}

#[test]
fn given_no_member_identity_when_shared_extension_loads_then_it_registers_nothing() {
    let api: Arc<dyn ExtensionApi> = Arc::new(NoopApi);
    let result = register_member_extension(
        &api,
        MemberExtensionOptions {
            env: HashMap::new(),
            create_event_appender: Box::new(|_| panic!("appender must not be created")),
            scheduler: None,
        },
    );
    assert!(result.is_ok());
}
