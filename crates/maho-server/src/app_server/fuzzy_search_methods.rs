use super::{fuzzy_search_service::FuzzyFileSearchService, registry::{JsonRpcError, MethodRegistration, MethodRegistry, MethodScope}};
use serde_json::{Value, json};
use std::sync::Arc;

fn string(params: &Value, name: &str, method: &str) -> Result<String, JsonRpcError> {
    params[name].as_str().map(str::to_owned).ok_or_else(|| JsonRpcError::new(-32600, format!("{method} {name} must be a string")))
}
fn roots(params: &Value, method: &str) -> Result<Vec<String>, JsonRpcError> {
    params["roots"].as_array().and_then(|values| values.iter().map(|value| value.as_str().map(str::to_owned)).collect()).ok_or_else(|| JsonRpcError::new(-32600, format!("{method} roots must be an array of strings")))
}
pub fn register_fuzzy_file_search_methods(registry: &mut MethodRegistry, service: FuzzyFileSearchService) {
    for method in ["fuzzyFileSearch", "fuzzyFileSearch/sessionStart", "fuzzyFileSearch/sessionUpdate", "fuzzyFileSearch/sessionStop"] {
        let service = service.clone();
        registry.register(method.into(), MethodRegistration { requires_init:true, experimental:method != "fuzzyFileSearch", scope:MethodScope::Global, handler:Arc::new(move |context| {
            let service = service.clone(); Box::pin(async move {
                let params = &context.request["params"];
                if !params.is_object() { return Err(JsonRpcError::new(-32600, format!("{method} params must be an object"))); }
                match method {
                    "fuzzyFileSearch" => {
                        let query = string(params, "query", method)?;
                        let roots = roots(params, method)?;
                        let token = match params.get("cancellationToken") {
                            None | Some(Value::Null) => None,
                            Some(Value::String(token)) => Some(token.clone()),
                            _ => return Err(JsonRpcError::new(-32600, "fuzzyFileSearch cancellationToken must be a string or null")),
                        };
                        Ok(json!({"files":service.search(&query, roots, token).await}))
                    },
                    "fuzzyFileSearch/sessionStart" => { service.start_session(string(params,"sessionId",method)?, roots(params,method)?)?; Ok(json!({})) },
                    "fuzzyFileSearch/sessionUpdate" => { service.update_session(string(params,"sessionId",method)?, string(params,"query",method)?)?; Ok(json!({})) },
                    "fuzzyFileSearch/sessionStop" => { service.stop_session(&string(params,"sessionId",method)?); Ok(json!({})) },
                    _ => unreachable!(),
                }
            })
        }) });
    }
}
