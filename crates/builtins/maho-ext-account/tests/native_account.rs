#[path = "native_account/support.rs"]
pub mod support;
use maho_core::{auth_storage::AuthStorage, credential_accounts, credential_pool::state_store::CredentialSlotRepository};
use maho_ext_api::*;
use maho_ext_host::{ExtensionRunner, loader::{load_extensions, NativeExtensionFactory}};
use serde_json::json;
use std::sync::{Arc, Mutex};

#[tokio::test]
async fn registered_account_command_mutates_persisted_synthetic_storage() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("auth.json");
    let storage = AuthStorage::create(&path.to_string_lossy());
    storage.set("synthetic", Some(json!({"type":"api_key","key":"synthetic-secret","accounts":[{"name":"first","key":"synthetic-secret","source":"login"},{"name":"second","key":"synthetic-secret","source":"import"}]}))).map_err(ExtensionFailure::from)?;
    let repository = Arc::new(CredentialSlotRepository::new(&root.path().join("pool.json").to_string_lossy()));
    let registry = Arc::new(support::SyntheticRegistry {storage:Arc::new(tokio::sync::Mutex::new(storage)), repository:repository.clone()});
    let ui = Arc::new(support::TestUi::default());
    let loaded = load_extensions(vec![NativeExtensionFactory {path:"<account-native>".into(),source_info:SourceInfo::default(),extension:Box::new(maho_ext_account::Account)}],root.path(),ExtensionSessionProfile::default());
    assert!(loaded.errors.is_empty());
    let runner = ExtensionRunner::new(loaded.extensions,loaded.runtime,loaded.events,support::context(registry.clone(),ui.clone()));
    let context = runner.create_command_context(Arc::new(support::CommandActions(Mutex::new(Vec::new()))))?;
    let outcome = async {
        for (command, pinned, display, count) in [
            ("synthetic list",false,None,2),
            ("synthetic pin second",true,None,2),
            ("synthetic unpin",false,None,2),
            ("synthetic rename second Work Account",false,Some("Work Account"),2),
            ("synthetic clear-name second",false,None,2),
            ("synthetic remove second",false,None,1),
        ] {
            runner.invoke_command("account",command,&context).await?;
            let reopened = AuthStorage::create(&path.to_string_lossy());
            let accounts = credential_accounts::get_credential_accounts(&reopened,"synthetic",&|_|None,&repository,0).await.map_err(ExtensionFailure::from)?;
            assert_eq!(accounts.len(),count);
            if count==2 { assert_eq!(accounts[1].pinned,pinned); assert_eq!(accounts[1].display_name.as_deref(),display); }
            let notifications = ui.0.lock().expect("notifications");
            assert_eq!(notifications.last().expect("command notification").1,NotificationType::Info);
            assert!(!notifications.iter().any(|(text,_)|text.contains("synthetic-secret")));
        }
        runner.invoke_command("account","synthetic remove missing",&context).await?;
        assert_eq!(ui.0.lock().expect("notifications").last().expect("error notification").1,NotificationType::Error);
        runner.invoke_command("account","anthropic list",&context).await?;
        assert_eq!(ui.0.lock().expect("notifications").last().expect("environment list").1,NotificationType::Info);
        for command in ["anthropic remove env", "anthropic rename env Environment", "synthetic pin missing", "synthetic clear-name second extra", "", "synthetic unknown"] {
            let before = ui.0.lock().expect("notifications").len();
            runner.invoke_command("account",command,&context).await?;
            let notifications = ui.0.lock().expect("notifications");
            assert_eq!(notifications.len(),before+1);
            assert_eq!(notifications.last().expect("error notification").1,NotificationType::Error);
            assert!(!notifications.iter().any(|(text,_)|text.contains("synthetic-secret")||text.contains("synthetic-env-secret")));
        }
        Ok::<(),Box<dyn std::error::Error>>(())
    }.await;
    drop(context);
    drop(runner);
    drop(registry);
    root.close()?;
    outcome
}

#[tokio::test]
async fn registered_gpt_account_uses_shared_storage_and_requires_ui_for_login()->Result<(),Box<dyn std::error::Error>>{
    use maho_ext_builtin_loose::gpt_account::{GptAccount,PROVIDER_ID};
    use std::sync::atomic::{AtomicUsize,Ordering};
    let root=tempfile::tempdir()?;
    let path=root.path().join("auth.json");
    let storage=AuthStorage::create(&path.to_string_lossy());
    storage.set(PROVIDER_ID,Some(json!({"type":"api_key","key":"fixture-secret","accounts":[{"name":"first","key":"fixture-secret","source":"login"},{"name":"second","key":"fixture-secret","source":"import"}]}))).map_err(ExtensionFailure::from)?;
    let repository=Arc::new(CredentialSlotRepository::new(&root.path().join("pool.json").to_string_lossy()));
    let registry=Arc::new(support::SyntheticRegistry{storage:Arc::new(tokio::sync::Mutex::new(storage)),repository:repository.clone()});
    let ui=Arc::new(support::TestUi::default());
    let login_calls=Arc::new(AtomicUsize::new(0));
    let calls=login_calls.clone();let login_storage=registry.storage.clone();
    let extension=GptAccount{open_browser:Arc::new(|_|panic!("unexpected browser")),login:Arc::new(move|_|{
        let calls=calls.clone();let storage=login_storage.clone();
        Box::pin(async move{
            calls.fetch_add(1,Ordering::SeqCst);
            storage.lock().await.set(PROVIDER_ID,Some(json!({"type":"api_key","key":"fixture-secret","accounts":[{"name":"committed","key":"fixture-secret","source":"login"}]}))).map_err(ExtensionFailure::from)?;
            Ok(Some(maho_ai::auth::types::AccountLoginReceipt{provider_id:PROVIDER_ID.into(),name:"committed".into(),origin:maho_ai::auth::types::AccountLoginOrigin::Generated}))
        })
    })};
    let loaded=load_extensions(vec![NativeExtensionFactory{path:"<gpt-account-native>".into(),source_info:SourceInfo::default(),extension:Box::new(extension)}],root.path(),ExtensionSessionProfile::default());
    let runner=ExtensionRunner::new(loaded.extensions,loaded.runtime,loaded.events,support::context(registry.clone(),ui.clone()));
    let mut context=runner.create_command_context(Arc::new(support::CommandActions(Mutex::new(Vec::new()))))?;
    let outcome=async{
        for (command,pinned,display,count) in [("list",false,None,2),("pin second",true,None,2),("unpin",false,None,2),("rename second Work",false,Some("Work"),2),("clear-name second",false,None,2),("remove second",false,None,1)]{
            runner.invoke_command("gpt-account",command,&context).await?;
            let reopened=AuthStorage::create(&path.to_string_lossy());
            let accounts=credential_accounts::get_credential_accounts(&reopened,PROVIDER_ID,&|_|None,&repository,0).await.map_err(ExtensionFailure::from)?;
            assert_eq!(accounts.len(),count);
            if count==2{assert_eq!(accounts[1].pinned,pinned);assert_eq!(accounts[1].display_name.as_deref(),display);}
            assert_eq!(ui.0.lock().expect("notifications").last().expect("notification").1,NotificationType::Info);
        }
        runner.invoke_command("gpt-account","add",&context).await?;
        assert_eq!(login_calls.load(Ordering::SeqCst),0);
        assert_eq!(ui.0.lock().expect("notifications").last().expect("headless refusal").1,NotificationType::Error);
        context.context.has_ui=true;
        runner.invoke_command("gpt-account","add",&context).await?;
        assert_eq!(login_calls.load(Ordering::SeqCst),1);
        let reopened=AuthStorage::create(&path.to_string_lossy());
        let accounts=credential_accounts::get_credential_accounts(&reopened,PROVIDER_ID,&|_|None,&repository,0).await.map_err(ExtensionFailure::from)?;
        assert_eq!(accounts.len(),1);assert_eq!(accounts[0].name,"committed");assert!(accounts[0].display_name.is_none());
        assert!(!ui.0.lock().expect("notifications").iter().any(|(text,_)|text.contains("fixture-secret")));
        Ok::<(),Box<dyn std::error::Error>>(())
    }.await;
    drop(context);drop(runner);drop(registry);root.close()?;
    outcome
}
