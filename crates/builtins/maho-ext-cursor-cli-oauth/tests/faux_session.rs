use std::sync::Arc;
use maho_ai::auth::{credential_store::InMemoryCredentialStore, types::{Credential, CredentialStore}};
use maho_ext_cursor_cli_oauth::{accounts::{CursorCliAccountSlot, AccountSource, add_account, empty_credential}, extension::CursorCliExtension, oauth_login::CursorCliOAuth, settings::CursorCliOauthProviderSettings};
use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::{faux::FauxScript, faux_session::FauxSession};

#[tokio::test]
async fn retired_native_import_cannot_write_after_waiting_for_store_lock() {
    for action in ["import native","remove work","pin work","unpin"] {
    let store=Arc::new(InMemoryCredentialStore::new());
    let credential=add_account(&empty_credential(),CursorCliAccountSlot {name:"work".into(),display_name:None,access:"fixture".into(),refresh:"fixture".into(),expires:10000.0,source:AccountSource::Login,blocked_until:None,block_reason:None}).expect("slot");
    let credential=maho_ext_cursor_cli_oauth::accounts::pin_account(&credential,"work").expect("pin");
    let expected=Credential::OAuth(credential.clone());
    store.modify("cursor-cli-oauth",Box::new(move |_|Box::pin(async move {Ok(Some(Credential::OAuth(credential)))})),None).await.expect("provider seed");
    store.modify("cursor",Box::new(|_|Box::pin(async {Ok(Some(Credential::OAuth(maho_ai::auth::types::OAuthCredential::new("fixture","fixture",1000.0))))})),None).await.expect("native seed");
    let oauth=Arc::new(CursorCliOAuth {store:store.clone(),flow:Arc::new(maho_ai::auth::oauth::cursor::CursorOAuth::new()),settings:Arc::new(CursorCliOauthProviderSettings::default),resolve:Arc::new(|_|Ok(())),persist_acknowledgement:Arc::new(|_|Ok(())),persist_enabled:Arc::new(|_|panic!("retired import cannot enable provider")),now:Arc::new(||1)});
    struct RetiredImport {extension:CursorCliExtension,store:Arc<InMemoryCredentialStore>,action:&'static str}
    impl maho_ext_api::Extension for RetiredImport {
        fn register(&self,api:&mut maho_ext_api::ExtensionApi) {
            self.extension.register(api);
            let handler=api.registered.commands.iter().find(|command|command.name=="cursor-account").expect("account command").handler.clone();
            let runtime=api.runtime.clone();let store=self.store.clone();let action=self.action;
            api.register_command("assert-retired-import",None,None,Arc::new(move |_,ctx| {
                let handler=handler.clone();let runtime=runtime.clone();let store=store.clone();Box::pin(async move {
                    let before=store.read("cursor-cli-oauth",None).await.expect("read before lock");
                    let (entered,entry)=tokio::sync::oneshot::channel();let (release,released)=tokio::sync::oneshot::channel();
                    let locked=store.clone();let task=tokio::spawn(async move {locked.modify("cursor-cli-oauth",Box::new(move |current|Box::pin(async move {entered.send(()).expect("lock entry");released.await.expect("release");Ok(current)})),None).await});
                    tokio::time::timeout(std::time::Duration::from_secs(5),entry).await.expect("bounded lock entry").expect("entered");
                    let mut import=handler(action,ctx);
                    std::future::poll_fn(|cx| {assert!(import.as_mut().poll(cx).is_pending(),"import waits for held lock");std::task::Poll::Ready(())}).await;
                    runtime.invalidate("generation retired during import");release.send(()).expect("release lock");
                    tokio::time::timeout(std::time::Duration::from_secs(5),task).await.expect("bounded lock release").expect("join").expect("mutation");
                    assert!(tokio::time::timeout(std::time::Duration::from_secs(5),import).await.expect("bounded import").is_err());
                    assert_eq!(serde_json::to_value(store.read("cursor-cli-oauth",None).await.expect("read")).expect("after"),serde_json::to_value(before).expect("before"));
                    Ok(())
                })
            }));
        }
    }
    let extension=CursorCliExtension {oauth,settings:Arc::new(CursorCliOauthProviderSettings::default),stream:Arc::new(|_,_,_|panic!("command cannot stream")),catalog:None,router:None,shutdown:None,catalog_refresh:None};
    let session=FauxSession::new(FauxScript {name:"retired-import".into(),prompt:"/assert-retired-import".into(),responses:Vec::new()}).with_native_extension(NativeExtensionFactory {path:"<cursor-cli-oauth>".into(),source_info:Default::default(),extension:Box::new(RetiredImport {extension,store:store.clone(),action})});
    tokio::time::timeout(std::time::Duration::from_secs(15),session.run_native()).await.expect("bounded scenario").expect("retired import scenario");
    assert_eq!(serde_json::to_value(store.read("cursor-cli-oauth",None).await.expect("read")).expect("actual"),serde_json::to_value(Some(expected)).expect("expected"));
    }
}

#[tokio::test]
async fn malformed_catalog_credentials_keep_offline_registration_available() {
    let directory=tempfile::tempdir().expect("directory");
    let store=Arc::new(InMemoryCredentialStore::new());
    let mut credential=empty_credential();credential.extra.insert("accounts".into(),serde_json::json!([{"name":"invalid"}]));
    store.modify("cursor-cli-oauth",Box::new(move |_|Box::pin(async move {Ok(Some(Credential::OAuth(credential)))})),None).await.expect("seed malformed pool");
    let oauth=Arc::new(CursorCliOAuth {store,flow:Arc::new(maho_ai::auth::oauth::cursor::CursorOAuth::new()),settings:Arc::new(||CursorCliOauthProviderSettings {enabled:true,..Default::default()}),resolve:Arc::new(|_|Ok(())),persist_acknowledgement:Arc::new(|_|Ok(())),persist_enabled:Arc::new(|_|Ok(())),now:Arc::new(||1)});
    struct OfflineAssertion(CursorCliExtension);
    impl maho_ext_api::Extension for OfflineAssertion {
        fn register(&self,api:&mut maho_ext_api::ExtensionApi) {
            self.0.register(api);
            let refresh=self.0.catalog_refresh.clone().expect("native refresh");
            api.register_command("assert-offline",None,None,Arc::new(move |_,ctx| {
                let task=refresh.lock().expect("catalog task").take().expect("scheduled refresh");Box::pin(async move {
                tokio::time::timeout(std::time::Duration::from_secs(5),task).await.expect("bounded failed probe").expect("refresh task");
                assert!(ctx.model_registry.find("cursor-cli-oauth","auto").is_some());
                Ok(())
            })}));
        }
    }
    let extension=CursorCliExtension::native(oauth,directory.path().join("missing"),directory.path().into(),directory.path().join("agent"),Default::default());
    let session=FauxSession::new(FauxScript {name:"cursor-offline".into(),prompt:"/assert-offline".into(),responses:Vec::new()}).with_native_extension(NativeExtensionFactory {path:"<cursor-cli-oauth>".into(),source_info:Default::default(),extension:Box::new(OfflineAssertion(extension))});
    tokio::time::timeout(std::time::Duration::from_secs(10),session.run_native()).await.expect("bounded startup").expect("offline provider survives malformed credentials");
}

#[tokio::test]
async fn catalog_startup_does_not_wait_for_locked_native_bootstrap() {
    let directory=tempfile::tempdir().expect("directory");let store=Arc::new(InMemoryCredentialStore::new());
    store.modify("cursor",Box::new(|_|Box::pin(async {Ok(Some(Credential::OAuth(maho_ai::auth::types::OAuthCredential::new("fixture","fixture",1000.0))))})),None).await.expect("native seed");
    let (entered,entry)=tokio::sync::oneshot::channel();let (release,released)=tokio::sync::oneshot::channel();let locked=store.clone();
    let holder=tokio::spawn(async move {locked.modify("cursor-cli-oauth",Box::new(move |current|Box::pin(async move {entered.send(()).expect("entry");released.await.expect("release");Ok(current)})),None).await});
    tokio::time::timeout(std::time::Duration::from_secs(5),entry).await.expect("bounded lock entry").expect("entered");
    let oauth=Arc::new(CursorCliOAuth {store:store.clone(),flow:Arc::new(maho_ai::auth::oauth::cursor::CursorOAuth::new()),settings:Arc::new(||CursorCliOauthProviderSettings {enabled:true,..Default::default()}),resolve:Arc::new(|_|Ok(())),persist_acknowledgement:Arc::new(|_|Ok(())),persist_enabled:Arc::new(|_|Ok(())),now:Arc::new(||1)});
    struct ReleaseBootstrap {extension:CursorCliExtension,release:std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>}
    impl maho_ext_api::Extension for ReleaseBootstrap {
        fn register(&self,api:&mut maho_ext_api::ExtensionApi) {
            self.extension.register(api);
            let release=Arc::new(std::sync::Mutex::new(self.release.lock().expect("release").take()));let refresh=self.extension.catalog_refresh.clone().expect("native refresh");
            api.register_command("release-bootstrap",None,None,Arc::new(move |_,ctx| {
                let release=release.lock().expect("release").take().expect("single invocation");let task=refresh.lock().expect("catalog task").take().expect("scheduled refresh");
                Box::pin(async move {
                    assert!(ctx.model_registry.find("cursor-cli-oauth","auto").is_some(),"fallback is available while bootstrap waits");
                    assert!(!task.is_finished(),"held store lock prevents refresh completion");
                    release.send(()).expect("release bootstrap");
                    tokio::time::timeout(std::time::Duration::from_secs(5),task).await.expect("bounded refresh").expect("refresh task");Ok(())
                })
            }));
        }
    }
    let extension=CursorCliExtension::native(oauth,directory.path().join("missing"),directory.path().into(),directory.path().join("agent"),Default::default());
    let session=FauxSession::new(FauxScript {name:"nonblocking-catalog".into(),prompt:"/release-bootstrap".into(),responses:Vec::new()}).with_native_extension(NativeExtensionFactory {path:"<cursor-cli-oauth>".into(),source_info:Default::default(),extension:Box::new(ReleaseBootstrap {extension,release:std::sync::Mutex::new(Some(release))})});
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),session.run_native()).await;
    drop(session);
    let release_result=tokio::time::timeout(std::time::Duration::from_secs(5),holder).await.expect("bounded lock cleanup").expect("holder join");release_result.expect("holder mutation");
    result.expect("startup must reach command without waiting for bootstrap").expect("scenario");
    assert!(store.read("cursor-cli-oauth",None).await.expect("read").is_some());
}

#[tokio::test]
async fn native_startup_probes_account_home_and_replaces_offline_catalog() {
    use std::os::unix::fs::PermissionsExt;
    let directory=tempfile::tempdir().expect("dir");let executable=directory.path().join("cursor-agent");
    std::fs::write(&executable,"#!/bin/sh\n[ \"$1\" = models ] || exit 9\n[ -f \"$HOME/.cursor/auth.json\" ] || exit 8\nprintf 'model-a - Model A\\n'\n").expect("script");
    std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o700)).expect("permissions");
    let store=Arc::new(InMemoryCredentialStore::new());
    let credential=add_account(&empty_credential(),CursorCliAccountSlot {name:"work".into(),display_name:None,access:"fixture".into(),refresh:"fixture".into(),expires:999999.0,source:AccountSource::Login,blocked_until:None,block_reason:None}).expect("slot");
    store.modify("cursor-cli-oauth",Box::new(move |_|Box::pin(async move {Ok(Some(Credential::OAuth(credential)))})),None).await.expect("seed");
    let oauth=Arc::new(CursorCliOAuth {store,flow:Arc::new(maho_ai::auth::oauth::cursor::CursorOAuth::new()),settings:Arc::new(||CursorCliOauthProviderSettings {enabled:true,..Default::default()}),resolve:Arc::new(|_|Ok(())),persist_acknowledgement:Arc::new(|_|Ok(())),persist_enabled:Arc::new(|_|Ok(())),now:Arc::new(||1)});
    let agent_dir=directory.path().join("agent");
    let extension=CursorCliExtension::native(oauth,executable,directory.path().into(),agent_dir.clone(),Default::default());
    struct CatalogAssertion(CursorCliExtension);
    impl maho_ext_api::Extension for CatalogAssertion {
        fn register(&self,api:&mut maho_ext_api::ExtensionApi) {
            self.0.register(api);
            let shutdown_handler=api.registered.handlers[&maho_ext_api::EventKind::SessionShutdown][0].clone();
            let startup_handler=api.registered.handlers[&maho_ext_api::EventKind::SessionStart][0].clone();
            let shutdown=self.0.shutdown.clone().expect("generation");
            let retired_refresh=self.0.catalog_refresh.clone().expect("catalog owner");
            api.register_command("retire-generation",None,None,Arc::new(move |_,ctx| {
                let handler=shutdown_handler.clone();let startup=startup_handler.clone();let shutdown=shutdown.clone();let refresh=retired_refresh.clone();Box::pin(async move {
                    let mut event=maho_ext_api::ExtensionEvent::SessionShutdown(maho_ext_api::SessionShutdownEvent {reason:maho_ext_api::SessionReason::Quit,target_session_file:None,signal:None});
                    handler(&mut event,ctx).await?;
                    assert!(shutdown.signal().aborted(),"registered shutdown handler must retire its generation");
                    assert!(refresh.lock().expect("catalog task").is_none(),"shutdown joins the owned refresh");
                    let mut start=maho_ext_api::ExtensionEvent::SessionStart(maho_ext_api::SessionStartEvent {reason:maho_ext_api::SessionReason::Reload,initial_model_provenance:None,previous_session_file:None});
                    startup(&mut start,ctx).await?;
                    assert!(refresh.lock().expect("catalog task").is_none(),"retired generation cannot schedule another probe");
                    Ok(())
                })
            }));
            let refresh=self.0.catalog_refresh.clone().expect("native catalog task");
            api.register_command("assert-catalog",None,None,Arc::new(move |_,ctx| {
                let task=refresh.lock().expect("catalog task").take().expect("startup scheduled probe");
                Box::pin(async move {
                tokio::time::timeout(std::time::Duration::from_secs(5),task).await.expect("bounded catalog publication").expect("catalog task");
                assert!(ctx.model_registry.find("cursor-cli-oauth","model-a").is_some(),"startup must publish the probed model to the bound host registry");
                Ok(())
            })}));
        }
    }
    let session=FauxSession::new(FauxScript {name:"cursor-startup".into(),prompt:"/assert-catalog".into(),responses:Vec::new()}).with_native_extension(NativeExtensionFactory {path:"<cursor-cli-oauth>".into(),source_info:Default::default(),extension:Box::new(CatalogAssertion(extension))});
    tokio::time::timeout(std::time::Duration::from_secs(10),session.run_native()).await.expect("bounded startup").expect("startup");
    let cache:serde_json::Value=serde_json::from_str(&std::fs::read_to_string(agent_dir.join("cursor-cli-oauth/models.json")).expect("catalog cache")).expect("cache JSON");
    assert_eq!(cache["models"][0]["id"],"model-a");
    let retirement=FauxSession::new(FauxScript {name:"cursor-retirement".into(),prompt:"/retire-generation".into(),responses:Vec::new()}).with_native_extension(NativeExtensionFactory {path:"<cursor-cli-oauth>".into(),source_info:Default::default(),extension:Box::new(CatalogAssertion(CursorCliExtension::native(Arc::new(CursorCliOAuth {store:Arc::new(InMemoryCredentialStore::new()),flow:Arc::new(maho_ai::auth::oauth::cursor::CursorOAuth::new()),settings:Arc::new(CursorCliOauthProviderSettings::default),resolve:Arc::new(|_|Ok(())),persist_acknowledgement:Arc::new(|_|Ok(())),persist_enabled:Arc::new(|_|Ok(())),now:Arc::new(||1)}),directory.path().join("missing"),directory.path().into(),agent_dir,Default::default())))});
    tokio::time::timeout(std::time::Duration::from_secs(10),retirement.run_native()).await.expect("bounded retirement").expect("retirement");
}

#[tokio::test]
async fn native_builtin_account_commands_mutate_bound_credentials_without_streaming() {
    let store = Arc::new(InMemoryCredentialStore::new());
    let credential = add_account(&empty_credential(), CursorCliAccountSlot { name: "work".into(), display_name: None, access: "synthetic".into(), refresh: "synthetic".into(), expires: 999999.0, source: AccountSource::Login, blocked_until: Some(123.0), block_reason: Some(maho_ext_cursor_cli_oauth::accounts::BlockReason::RateLimit) }).expect("slot");
    store.modify("cursor-cli-oauth", Box::new(move |_| Box::pin(async move { Ok(Some(Credential::OAuth(credential))) })), None).await.expect("seed");
    store.modify("cursor", Box::new(|_| Box::pin(async { Ok(Some(Credential::OAuth(maho_ai::auth::types::OAuthCredential::new("fixture", "fixture", 1000.0)))) })), None).await.expect("native seed");
    for prompt in ["/cursor-account rename work Primary account", "/cursor-account clear-name work", "/cursor-account pin work", "/cursor-account remove work", "/cursor-account import native"] {
        let settings = Arc::new(CursorCliOauthProviderSettings::default);
        let oauth = Arc::new(CursorCliOAuth { store: store.clone(), flow: Arc::new(maho_ai::auth::oauth::cursor::CursorOAuth::new()), settings: settings.clone(), resolve: Arc::new(|_| Ok(())), persist_acknowledgement: Arc::new(|_| Ok(())), persist_enabled: Arc::new(|_| Ok(())), now: Arc::new(|| 1) });
        let session = FauxSession::new(FauxScript { name: "cursor-account".into(), prompt: prompt.into(), responses: Vec::new() }).with_native_extension(NativeExtensionFactory { path: "<cursor-cli-oauth>".into(), source_info: Default::default(), extension: Box::new(CursorCliExtension { oauth, settings, stream: Arc::new(|_, _, _| panic!("command must not stream")),catalog:None,router:None,shutdown:None,catalog_refresh:None }) });
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native()).await.expect("bounded command").expect("native builtin");
        assert_eq!(result["messages"], serde_json::json!([]));
        let credential = store.read("cursor-cli-oauth", None).await.expect("read").expect("credential").into_oauth().expect("oauth");
        if prompt.contains("rename work") { assert_eq!(credential.extra["accounts"][0]["displayName"], "Primary account"); assert_eq!(credential.extra["accounts"][0]["blockedUntil"],123.0); assert_eq!(credential.extra["accounts"][0]["blockReason"],"rate_limit"); }
        else if prompt.contains("clear-name") { assert!(credential.extra["accounts"][0].get("displayName").is_none()); }
        else if prompt.contains("import native") { assert_eq!(maho_ext_cursor_cli_oauth::accounts::list_accounts(&credential).expect("slots")[0].name,"native"); }
        else if prompt.contains("pin work") { assert_eq!(credential.get_extra_str("pinned"), Some("work")); } else { assert!(credential.get_extra_str("pinned").is_none()); assert!(maho_ext_cursor_cli_oauth::accounts::list_accounts(&credential).expect("slots").is_empty()); }
    }
}
