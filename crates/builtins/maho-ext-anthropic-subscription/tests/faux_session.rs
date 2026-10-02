use std::sync::Arc;
use maho_ai::auth::{credential_store::InMemoryCredentialStore, types::{Credential, CredentialStore}};
use maho_ext_anthropic_subscription::{accounts::{AccountSlot, AccountSource, add_account, empty_credential}, extension::AnthropicSubscriptionExtension, oauth_login::AnthropicSubscriptionOAuth};
use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::{faux::FauxScript, faux_session::FauxSession};

#[tokio::test]
async fn queued_registry_handler_rechecks_retirement_after_lock_acquisition() {
    struct RegistryAssertion;
    impl maho_ext_api::Extension for RegistryAssertion {
        fn register(&self,api:&mut maho_ext_api::ExtensionApi) {
            let registry=Arc::new(tokio::sync::Mutex::new(maho_ext_anthropic_subscription::session_stream::SessionRegistry::default()));
            maho_ext_anthropic_subscription::session_registry_wiring::register(api,registry.clone());
            let handler=api.registered.handlers[&maho_ext_api::EventKind::SessionStart][0].clone();let runtime=api.runtime.clone();
            api.register_command("assert-retired-registry",None,None,Arc::new(move |_,ctx| {
                let registry=registry.clone();let runtime=runtime.clone();let handler=handler.clone();Box::pin(async move {
                    let held=registry.lock().await;
                    let mut event=maho_ext_api::ExtensionEvent::SessionStart(maho_ext_api::SessionStartEvent {reason:maho_ext_api::SessionReason::New,initial_model_provenance:None,previous_session_file:None});
                    let mut pending=handler(&mut event,ctx);
                    std::future::poll_fn(|cx| {assert!(pending.as_mut().poll(cx).is_pending());std::task::Poll::Ready(())}).await;
                    runtime.invalidate("retired queued registry handler");drop(held);
                    assert!(tokio::time::timeout(std::time::Duration::from_secs(5),pending).await.expect("bounded handler").is_err());
                    assert!(registry.lock().await.entries.is_empty());Ok(())
                })
            }));
        }
    }
    let session=FauxSession::new(FauxScript {name:"retired-registry".into(),prompt:"/assert-retired-registry".into(),responses:Vec::new()}).with_native_extension(NativeExtensionFactory {path:"<registry-assertion>".into(),source_info:Default::default(),extension:Box::new(RegistryAssertion)});
    tokio::time::timeout(std::time::Duration::from_secs(10),session.run_native()).await.expect("bounded scenario").expect("scenario");
}

#[tokio::test]
async fn retired_account_mutations_cannot_write_after_waiting_for_store_lock() {
    for action in ["pin work", "unpin", "remove work"] {
        let store=Arc::new(InMemoryCredentialStore::new());
        let credential=add_account(&empty_credential(),AccountSlot {name:"work".into(),display_name:None,access:"fixture".into(),refresh:"fixture".into(),expires:10000.0,source:AccountSource::Login,blocked_until:None,block_reason:None}).expect("slot");
        let credential=maho_ext_anthropic_subscription::accounts::pin_account(&credential,"work");
        store.modify("anthropic-subscription",Box::new(move |_|Box::pin(async move {Ok(Some(Credential::OAuth(credential)))})),None).await.expect("seed");
        struct RetiredMutation {store:Arc<InMemoryCredentialStore>,action:&'static str}
        impl maho_ext_api::Extension for RetiredMutation {
            fn register(&self,api:&mut maho_ext_api::ExtensionApi) {
                let settings=Arc::new(||maho_ext_anthropic_subscription::settings::load(&serde_json::json!({}),&serde_json::Value::Null,&Default::default()));
                let oauth=Arc::new(AnthropicSubscriptionOAuth {store:self.store.clone(),flow:Arc::new(maho_ai::auth::oauth::anthropic::AnthropicOAuth::new(maho_ai::auth::oauth::transport::default_transport())),settings,ambient:maho_ext_anthropic_subscription::availability::AmbientAuthStatusReader::new(Arc::new(||Box::pin(async {Ok(false)})),Arc::new(||1),30_000)});
                maho_ext_anthropic_subscription::account_command::register(api,oauth,Arc::new(Default::default()));
                let handler=api.registered.commands.iter().find(|command|command.name=="claude-account").expect("command").handler.clone();
                let runtime=api.runtime.clone();let store=self.store.clone();let action=self.action;
                api.register_command("assert-retired-mutation",None,None,Arc::new(move |_,ctx| {
                    let handler=handler.clone();let runtime=runtime.clone();let store=store.clone();Box::pin(async move {
                        let before=serde_json::to_value(store.read("anthropic-subscription",None).await.expect("before")).expect("serialize");
                        let (entered,entry)=tokio::sync::oneshot::channel();let (release,released)=tokio::sync::oneshot::channel();
                        let locked=store.clone();let holder=tokio::spawn(async move {locked.modify("anthropic-subscription",Box::new(move |current|Box::pin(async move {entered.send(()).expect("entry");released.await.expect("release");Ok(current)})),None).await});
                        tokio::time::timeout(std::time::Duration::from_secs(5),entry).await.expect("bounded entry").expect("entered");
                        let mut mutation=handler(action,ctx);
                        std::future::poll_fn(|cx| {assert!(mutation.as_mut().poll(cx).is_pending());std::task::Poll::Ready(())}).await;
                        runtime.invalidate("retired while mutation queued");release.send(()).expect("release");
                        tokio::time::timeout(std::time::Duration::from_secs(5),holder).await.expect("bounded holder").expect("join").expect("mutation");
                        assert!(tokio::time::timeout(std::time::Duration::from_secs(5),mutation).await.expect("bounded command").is_err());
                        assert_eq!(serde_json::to_value(store.read("anthropic-subscription",None).await.expect("after")).expect("serialize"),before);
                        Ok(())
                    })
                }));
            }
        }
        let session=FauxSession::new(FauxScript {name:"retired-account".into(),prompt:"/assert-retired-mutation".into(),responses:Vec::new()}).with_native_extension(NativeExtensionFactory {path:"<anthropic-subscription>".into(),source_info:Default::default(),extension:Box::new(RetiredMutation {store,action})});
        tokio::time::timeout(std::time::Duration::from_secs(15),session.run_native()).await.expect("bounded scenario").expect("scenario");
    }
}

#[tokio::test]
async fn native_builtin_command_pins_and_removes_accounts_through_bound_session() {
    let store = Arc::new(InMemoryCredentialStore::new());
    let credential = add_account(&empty_credential(), AccountSlot { name: "work".into(), display_name: None, access: "synthetic".into(), refresh: "synthetic".into(), expires: 999999.0, source: AccountSource::Login, blocked_until: Some(123.0), block_reason: Some("rate_limit".into()) }).expect("slot");
    store.modify("anthropic-subscription", Box::new(move |_| Box::pin(async move { Ok(Some(Credential::OAuth(credential))) })), None).await.expect("seed");
    for prompt in ["/claude-account rename work Primary account", "/claude-account clear-name work", "/claude-account pin work", "/claude-account remove work"] {
        let settings = Arc::new(|| maho_ext_anthropic_subscription::settings::load(&serde_json::json!({}), &serde_json::Value::Null, &Default::default()));
        let oauth = Arc::new(AnthropicSubscriptionOAuth { store: store.clone(), flow: Arc::new(maho_ai::auth::oauth::anthropic::AnthropicOAuth::new(maho_ai::auth::oauth::transport::default_transport())), settings: settings.clone(), ambient: maho_ext_anthropic_subscription::availability::AmbientAuthStatusReader::new(Arc::new(|| Box::pin(async { Ok(false) })), Arc::new(|| 1), 30_000) });
        let session = FauxSession::new(FauxScript { name: "anthropic-account".into(), prompt: prompt.into(), responses: Vec::new() }).with_native_extension(NativeExtensionFactory { path: "<anthropic-subscription>".into(), source_info: Default::default(), extension: Box::new(AnthropicSubscriptionExtension { oauth, settings, registry: Arc::new(tokio::sync::Mutex::new(Default::default())), watch: Arc::new(std::sync::Mutex::new(Default::default())), stream: Arc::new(|_, _, _| panic!("account command must not stream")) }) });
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native()).await.expect("bounded command").expect("native builtin");
        assert_eq!(result["messages"], serde_json::json!([]));
        let credential = store.read("anthropic-subscription", None).await.expect("read").expect("credential").into_oauth().expect("oauth");
        if prompt.contains("rename work") { assert_eq!(credential.extra["accounts"][0]["displayName"], "Primary account"); assert_eq!(credential.extra["accounts"][0]["blockedUntil"],123.0); assert_eq!(credential.extra["accounts"][0]["blockReason"],"rate_limit"); }
        else if prompt.contains("clear-name") { assert!(credential.extra["accounts"][0].get("displayName").is_none()); }
        else if prompt.contains("pin work") { assert_eq!(credential.get_extra_str("pinned"), Some("work")); } else { assert!(credential.get_extra_str("pinned").is_none()); assert!(maho_ext_anthropic_subscription::accounts::list_accounts(&credential, None).expect("slots").is_empty()); }
    }
}
