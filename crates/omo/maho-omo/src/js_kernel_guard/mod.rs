//! OMO's eval-only `js-kernel-guard` component (#14023).

mod prelude;

use maho_ext_api::{
    Extension, ExtensionApi, ToolDefinition, ToolExecutionMode, ToolExposure, ToolResult,
};
use serde_json::json;
use std::sync::Arc;

const CELL_HINT: &str = concat!(
    "Fleet work: read the omo-relay skill before the first call. In a JS eval cell the fleet client is ",
    "const { bunshin } = await import(`${env(\"OMO_BUNSHIN_TS_SDK_ROOT\")}/sdk.js`). ",
    "Import only sdk.js. Never import scripts/node/bunshin-node.mjs ",
    "(that file is the node-agent executable; its top-level main() calls process.exit).",
);

pub(super) struct JsKernelGuardComponent;

impl Extension for JsKernelGuardComponent {
    fn register(&self, api: &mut ExtensionApi) {
        let mut tool = ToolDefinition::new(
            "js_kernel_guard",
            CELL_HINT,
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
            Arc::new(|_| {
                Box::pin(async {
                    Ok(ToolResult {
                        details: Some(json!({})),
                        ..ToolResult::text(CELL_HINT)
                    })
                })
            }),
        );
        tool.label = "JS kernel guard".into();
        tool.exposure = Some(ToolExposure::Eval);
        tool.execution_mode = Some(ToolExecutionMode::Parallel);
        tool.permission_parser = Some(Arc::new(|_, _| Vec::new()));
        tool.kernel_prelude = Some(prelude::js_kernel_guard_prelude());
        api.register_tool(tool);
    }
}
