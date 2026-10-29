//! Port of `omo-senpi/src/extension/tool-hook-status.ts` at pin `77f3067f1`.

use maho_ext_api::ExtensionContext;

/// Upstream reads a callable `updateToolHookStatus` off the event context and is a no-op when the
/// host does not expose one. Native hosts expose it as `ExtensionContext.update_tool_hook_status`.
pub fn report_tool_hook_status(ctx: &ExtensionContext, status_message: &str) {
    if let Some(update) = &ctx.update_tool_hook_status {
        update(status_message);
    }
}
