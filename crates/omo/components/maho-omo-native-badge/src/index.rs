use std::sync::Arc;
use maho_ext_api::{EventKind, EventResult, Extension, ExtensionApi};
use crate::footer_badge::NativeBadgeStatus;

pub struct NativeBadgeComponent;
impl NativeBadgeComponent { pub const NAME: &str = "native-badge"; }
impl Extension for NativeBadgeComponent {
    fn register(&self, api: &mut ExtensionApi) {
        for event in [EventKind::SessionStart, EventKind::AgentSettled] {
            api.on(event, Arc::new(|_, ctx| Box::pin(async move {
                NativeBadgeStatus.publish(Some(ctx.ui.as_ref()));
                Ok(EventResult::None)
            })));
        }
    }
}
