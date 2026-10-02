use maho_ext_api::{ExtensionApi,ExtensionFailure};
pub fn is_eval_only_routing(api:&ExtensionApi)->Result<bool,ExtensionFailure>{Ok(api.get_all_tools()?.iter().any(|tool|tool.name=="eval"))}
