use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}
impl JsonRpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}
#[derive(Clone, Default)]
pub struct RegistryConnection {
    pub initialized: bool,
    pub experimental_api: bool,
}
#[derive(Clone)]
pub struct HandlerContext {
    pub connection: RegistryConnection,
    pub request: Value,
}
pub type HandlerFuture = Pin<Box<dyn Future<Output = Result<Value, JsonRpcError>> + Send>>;
pub type Handler = Arc<dyn Fn(HandlerContext) -> HandlerFuture + Send + Sync>;
#[derive(Clone, Copy, Default)]
pub enum MethodScope {
    Thread,
    Global,
    #[default]
    None,
}
pub struct MethodRegistration {
    pub handler: Handler,
    pub experimental: bool,
    pub requires_init: bool,
    pub scope: MethodScope,
}
#[derive(Default)]
pub struct MethodRegistry {
    methods: BTreeMap<String, MethodRegistration>,
}
pub type ExtensionThreadResolver=Arc<dyn Fn(&str)->Result<Arc<maho_ext_host::ExtensionRunner>,JsonRpcError>+Send+Sync>;
pub fn register_extension_request_method(
    registry: &mut MethodRegistry,
    get_thread: ExtensionThreadResolver,
) {
    registry.register(
        "extension_request".into(),
        MethodRegistration {
            requires_init: true,
            experimental: false,
            scope: MethodScope::Thread,
            handler: Arc::new(move |context| {
                let get_thread = get_thread.clone();
                Box::pin(async move {
                    let params = &context.request["params"];
                    let thread_id = params
                        .get("threadId")
                        .and_then(Value::as_str)
                        .filter(|v| !v.is_empty())
                        .ok_or_else(|| JsonRpcError::new(-32602, "Invalid params"))?;
                    let name = params
                        .get("name")
                        .and_then(Value::as_str)
                        .filter(|v| !v.is_empty())
                        .ok_or_else(|| JsonRpcError::new(-32602, "Invalid params"))?;
                    let runner = get_thread(thread_id)
                        .map_err(|error| JsonRpcError::new(-32603, error.message))?;
                    runner
                        .handle_rpc_request(
                            name,
                            params.get("data").cloned().unwrap_or(Value::Null),
                        )
                        .await
                        .map_err(|error| JsonRpcError::new(-32603, error.message))
                })
            }),
        },
    );
}
impl MethodRegistry {
    pub fn register(&mut self, method: String, registration: MethodRegistration) {
        self.methods.insert(method, registration);
    }
    pub async fn dispatch(&self, connection: RegistryConnection, request: Value) -> Value {
        let id = request["id"].clone();
        let method = request["method"].as_str().unwrap_or_default();
        let registration = self.methods.get(method);
        let error = if !connection.initialized
            && (method != "initialize" || registration.is_none_or(|r| r.requires_init))
        {
            Some(JsonRpcError::new(-32600, "Not initialized"))
        } else if connection.initialized && method == "initialize" {
            Some(JsonRpcError::new(-32600, "Already initialized"))
        } else if registration.is_none() {
            Some(JsonRpcError::new(
                -32601,
                format!("Method not found: {method}"),
            ))
        } else if registration.is_some_and(|r| r.experimental) && !connection.experimental_api {
            Some(JsonRpcError::new(
                -32600,
                format!("{method} requires experimentalApi capability"),
            ))
        } else {
            None
        };
        if let Some(error) = error {
            return json!({"id":id,"error":error});
        }
        let Some(registration) = registration else {
            unreachable!("missing methods return above");
        };
        match (registration.handler)(HandlerContext {
            connection,
            request,
        })
        .await
        {
            Ok(result) => json!({"id":id,"result":result}),
            Err(error) => json!({"id":id,"error":error}),
        }
    }
}
