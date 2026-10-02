use maho_server::app_server::registry::*;
use serde_json::{Value, json};
use std::sync::Arc;
fn registration(result: Value, requires_init: bool, experimental: bool) -> MethodRegistration {
    MethodRegistration {
        handler: Arc::new(move |_| {
            let result = result.clone();
            Box::pin(async move { Ok(result) })
        }),
        requires_init,
        experimental,
        scope: MethodScope::None,
    }
}
#[tokio::test]
async fn initialization_gate_precedes_method_lookup() {
    let registry = MethodRegistry::default();
    let response = registry
        .dispatch(
            RegistryConnection::default(),
            json!({"id":1,"method":"missing"}),
        )
        .await;
    assert_eq!(response["error"]["message"], "Not initialized");
}
#[tokio::test]
async fn initialization_requires_explicit_exemption() {
    let mut registry = MethodRegistry::default();
    registry.register("initialize".into(), registration(json!({}), false, false));
    assert_eq!(
        registry
            .dispatch(
                RegistryConnection::default(),
                json!({"id":1,"method":"initialize"})
            )
            .await["result"],
        json!({})
    );
    assert_eq!(
        registry
            .dispatch(
                RegistryConnection {
                    initialized: true,
                    ..Default::default()
                },
                json!({"id":1,"method":"initialize"})
            )
            .await["error"]["message"],
        "Already initialized"
    );
}
#[tokio::test]
async fn experimental_gate_and_registration_replacement() {
    let mut registry = MethodRegistry::default();
    registry.register("test".into(), registration(json!(1), true, true));
    let request = json!({"id":"x","method":"test"});
    assert_eq!(
        registry
            .dispatch(
                RegistryConnection {
                    initialized: true,
                    experimental_api: false,
                    ..Default::default()
                },
                request.clone()
            )
            .await["error"]["code"],
        -32600
    );
    assert_eq!(
        registry
            .dispatch(
                RegistryConnection {
                    initialized: true,
                    experimental_api: true,
                    ..Default::default()
                },
                request.clone()
            )
            .await["result"],
        1
    );
    registry.register("test".into(), registration(json!(2), true, false));
    assert_eq!(
        registry
            .dispatch(
                RegistryConnection {
                    initialized: true,
                    experimental_api: false,
                    ..Default::default()
                },
                request
            )
            .await["result"],
        2
    );
}
#[tokio::test]
async fn intended_handler_error_preserves_data() {
    let mut registry = MethodRegistry::default();
    registry.register(
        "test".into(),
        MethodRegistration {
            handler: Arc::new(|_| {
                Box::pin(async {
                    Err(JsonRpcError {
                        code: -32602,
                        message: "Invalid params".into(),
                        data: Some(json!({"field":"name"})),
                    })
                })
            }),
            requires_init: true,
            experimental: false,
            scope: MethodScope::Thread,
        },
    );
    assert_eq!(
        registry
            .dispatch(
                RegistryConnection {
                    initialized: true,
                    ..Default::default()
                },
                json!({"id":null,"method":"test"})
            )
            .await,
        json!({"id":null,"error":{"code":-32602,"message":"Invalid params","data":{"field":"name"}}})
    );
}
