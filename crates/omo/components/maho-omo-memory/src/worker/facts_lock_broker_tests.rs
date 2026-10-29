#[cfg(all(test, unix))]
mod tests {
    use super::super::facts_lock_broker::{FactsLockBroker, FactsLockBrokerTask};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    use std::future::Future;

    fn assert_success(response:&serde_json::Value) { assert!(response["result"].get("Ok").is_some()); }

    #[tokio::test]
    async fn canonical_guard_excludes_native_contender_until_client_disconnect() {
        let root=tempfile::tempdir().unwrap();let agent=root.path().join("agent");let run=root.path().join("run");std::fs::create_dir(&agent).unwrap();std::fs::create_dir(&run).unwrap();
        let auth=agent.join("auth.json");std::fs::write(&auth,"{}\n").unwrap();let settings=agent.join("settings.json");std::fs::write(&settings,"{}\n").unwrap();
        std::fs::write(run.join("run-sentinel"),"preserve").unwrap();let broker=std::sync::Arc::new(FactsLockBroker::bind(&auth,&settings).unwrap());let endpoint=broker.endpoint().to_owned();let owned_dir=endpoint.parent().unwrap().to_owned();let (stop,receiver)=tokio::sync::watch::channel(false);let task=tokio::spawn(broker.serve(receiver));
        let mut client=tokio::net::UnixStream::connect(&endpoint).await.unwrap();client.write_all(b"{\"acquire\":\"auth\",\"budget_ms\":1000}\n").await.unwrap();let mut response=String::new();tokio::time::timeout(std::time::Duration::from_secs(2),tokio::io::BufReader::new(&mut client).read_line(&mut response)).await.unwrap().unwrap();assert_success(&serde_json::from_str(&response).unwrap());
        let auth_text=auth.to_string_lossy().into_owned();let blocked=tokio::task::spawn_blocking(move||maho_core::lockfile_policy::acquire_lock_sync(&auth_text,25)).await.unwrap();assert!(blocked.is_err());
        drop(client);let auth_text=auth.to_string_lossy().into_owned();let acquired=tokio::task::spawn_blocking(move||maho_core::lockfile_policy::acquire_lock_sync(&auth_text,1000)).await.unwrap();assert!(acquired.is_ok());drop(acquired);
        let _=stop.send(true);task.await.unwrap().unwrap();assert!(!endpoint.exists());assert!(!owned_dir.exists());assert_eq!(std::fs::read_to_string(run.join("run-sentinel")).unwrap(),"preserve");
    }

    #[tokio::test]
    async fn shutdown_closes_open_client_and_releases_guard() {
        let root=tempfile::tempdir().unwrap();let agent=root.path().join("agent");let run=root.path().join("run");std::fs::create_dir(&agent).unwrap();std::fs::create_dir(&run).unwrap();let auth=agent.join("auth.json");std::fs::write(&auth,"{}\n").unwrap();let settings=agent.join("settings.json");std::fs::write(&settings,"{}\n").unwrap();
        std::fs::write(run.join("run-sentinel"),"preserve").unwrap();let broker=std::sync::Arc::new(FactsLockBroker::bind(&auth,&settings).unwrap());let endpoint=broker.endpoint().to_owned();let owned_dir=endpoint.parent().unwrap().to_owned();let mut owner=FactsLockBrokerTask::start(broker);
        let (mut reader,mut writer)=tokio::net::UnixStream::connect(&endpoint).await.unwrap().into_split();writer.write_all(b"{\"acquire\":\"auth\",\"budget_ms\":1000}\n").await.unwrap();let mut response=String::new();tokio::time::timeout(std::time::Duration::from_secs(2),tokio::io::BufReader::new(&mut reader).read_line(&mut response)).await.unwrap().unwrap();assert_success(&serde_json::from_str(&response).unwrap());
        owner.close().await.unwrap();drop(reader);drop(writer);let auth_text=auth.to_string_lossy().into_owned();assert!(tokio::task::spawn_blocking(move||maho_core::lockfile_policy::acquire_lock_sync(&auth_text,1000)).await.unwrap().is_ok());assert!(!endpoint.exists());assert!(!owned_dir.exists());assert_eq!(std::fs::read_to_string(run.join("run-sentinel")).unwrap(),"preserve");
    }

    #[tokio::test]
    async fn close_cancellation_waits_for_inflight_blocking_acquire() {
        let root=tempfile::tempdir().unwrap();let agent=root.path().join("agent");let run=root.path().join("run");std::fs::create_dir(&agent).unwrap();std::fs::create_dir(&run).unwrap();let auth=agent.join("auth.json");std::fs::write(&auth,"{}\n").unwrap();let settings=agent.join("settings.json");std::fs::write(&settings,"{}\n").unwrap();
        let held=maho_core::lockfile_policy::acquire_lock_sync(&auth.to_string_lossy(),1000).unwrap();
        let (entered_tx,mut entered_rx)=tokio::sync::mpsc::unbounded_channel();let (release_tx,release_rx)=tokio::sync::oneshot::channel();let broker=std::sync::Arc::new(FactsLockBroker::bind(&auth,&settings).unwrap().with_acquire_gate(entered_tx,release_rx));let endpoint=broker.endpoint().to_owned();let mut owner=FactsLockBrokerTask::start(broker);
        let mut client=tokio::net::UnixStream::connect(&endpoint).await.unwrap();client.write_all(b"{\"acquire\":\"auth\",\"budget_ms\":1000}\n").await.unwrap();tokio::time::timeout(std::time::Duration::from_secs(2),entered_rx.recv()).await.unwrap().unwrap();
        let mut close=Box::pin(owner.close());
        std::future::poll_fn(|cx|match close.as_mut().poll(cx){std::task::Poll::Pending=>std::task::Poll::Ready(()),std::task::Poll::Ready(result)=>panic!("close completed before blocked acquisition settled: {result:?}")}).await;
        drop(close);drop(held);let _=release_tx.send(());
        tokio::time::timeout(std::time::Duration::from_secs(2),owner.close()).await.unwrap().unwrap();
        let auth_text=auth.to_string_lossy().into_owned();assert!(tokio::task::spawn_blocking(move||maho_core::lockfile_policy::acquire_lock_sync(&auth_text,1000)).await.unwrap().is_ok());
        assert!(!endpoint.exists());
    }

    #[tokio::test]
    async fn close_cancellation_waits_for_inflight_atomic_write() {
        let root=tempfile::tempdir().unwrap();let agent=root.path().join("agent");let run=root.path().join("run");std::fs::create_dir(&agent).unwrap();std::fs::create_dir(&run).unwrap();let auth=agent.join("auth.json");std::fs::write(&auth,"{}\n").unwrap();let settings=agent.join("settings.json");std::fs::write(&settings,"{}\n").unwrap();
        let (entered_tx,mut entered_rx)=tokio::sync::mpsc::unbounded_channel();let (release_tx,release_rx)=tokio::sync::oneshot::channel();let broker=std::sync::Arc::new(FactsLockBroker::bind(&auth,&settings).unwrap().with_write_gate(entered_tx,release_rx));let endpoint=broker.endpoint().to_owned();let mut owner=FactsLockBrokerTask::start(broker);
        let mut client=tokio::net::UnixStream::connect(&endpoint).await.unwrap();client.write_all(b"{\"acquire\":\"auth\",\"budget_ms\":1000}\n").await.unwrap();let mut response=String::new();tokio::time::timeout(std::time::Duration::from_secs(2),tokio::io::BufReader::new(&mut client).read_line(&mut response)).await.unwrap().unwrap();assert_success(&serde_json::from_str(&response).unwrap());
        client.write_all(b"{\"key\":\"auth\",\"write\":\"{\\\"provider\\\":{\\\"type\\\":\\\"api_key\\\",\\\"key\\\":\\\"changed\\\"}}\\n\"}\n").await.unwrap();tokio::time::timeout(std::time::Duration::from_secs(2),entered_rx.recv()).await.unwrap().unwrap();
        let mut close=Box::pin(owner.close());std::future::poll_fn(|cx|match close.as_mut().poll(cx){std::task::Poll::Pending=>std::task::Poll::Ready(()),std::task::Poll::Ready(result)=>panic!("close completed before atomic write settled: {result:?}")}).await;drop(close);
        let auth_text=auth.to_string_lossy().into_owned();let blocked=tokio::task::spawn_blocking(move||maho_core::lockfile_policy::acquire_lock_sync(&auth_text,25)).await.unwrap();assert!(blocked.is_err());let _=release_tx.send(());drop(client);
        tokio::time::timeout(std::time::Duration::from_secs(2),owner.close()).await.unwrap().unwrap();assert_eq!(std::fs::read_to_string(&auth).unwrap(),"{\"provider\":{\"type\":\"api_key\",\"key\":\"changed\"}}\n");assert!(!endpoint.exists());
    }

    #[tokio::test]
    async fn valid_and_absent_auth_initialization_preserve_canonical_guard() {
        let root=tempfile::tempdir().unwrap();let existing=root.path().join("valid-auth.json");std::fs::write(&existing,"{\"fixture\":{\"type\":\"api_key\",\"key\":\"kept\"}}\n").unwrap();
        maho_core::auth_storage::AuthStorage::prepare_facts_auth_file(&existing.to_string_lossy()).unwrap();assert!(std::fs::read_to_string(&existing).unwrap().contains("kept"));
        let missing=root.path().join("new").join("auth.json");maho_core::auth_storage::AuthStorage::prepare_facts_auth_file(&missing.to_string_lossy()).unwrap();assert_eq!(std::fs::read_to_string(&missing).unwrap(),"{}");
        let lock=missing.parent().unwrap().join("auth.json.lock");assert!(!lock.exists());
        #[cfg(unix)]{use std::os::unix::fs::PermissionsExt;assert_eq!(std::fs::metadata(&missing).unwrap().permissions().mode()&0o777,0o600);}
    }

    #[tokio::test]
    async fn brokered_selected_settings_source_writes_canonical_file_and_releases_guard() {
        let root=tempfile::tempdir().unwrap();let agent=root.path().join("agent");let run=root.path().join("run");std::fs::create_dir(&agent).unwrap();std::fs::create_dir(&run).unwrap();let settings=agent.join("settings.jsonc");std::fs::write(&settings,"{\"memory\":{\"enabled\":true}}\n").unwrap();let auth=agent.join("auth.json");std::fs::write(&auth,"{}\n").unwrap();
        let broker=std::sync::Arc::new(FactsLockBroker::bind(&auth,&settings).unwrap());let endpoint=broker.endpoint().to_owned();let config=maho_core::lockfile_policy::FactsLockBrokerConfig{endpoint:endpoint.clone(),auth_path:auth.clone(),settings_path:settings.clone()};let mut owner=FactsLockBrokerTask::start(broker);
        let cwd=run.to_string_lossy();let home=root.path().to_string_lossy();let agent_text=agent.to_string_lossy();let home_text=home.to_string();
        let manager=maho_core::settings_manager::SettingsManager::create(cwd.as_ref(),&agent_text,&home_text,false);
        assert_eq!(manager.settings_path(maho_core::settings_manager::SettingsScope::Global),Some(settings.to_string_lossy().as_ref()));
        let agent_text=agent.to_string_lossy().into_owned();let home_text=home.to_string();let cwd_text=cwd.to_string();let settings_for_write=settings.clone();tokio::task::spawn_blocking(move||{let _scope=maho_core::lockfile_policy::FactsLockBrokerScope::install(config);let storage=maho_core::settings_manager::FileSettingsStorage::new(&cwd_text,&agent_text,&home_text);use maho_core::settings_manager::SettingsStorage;storage.select_source(maho_core::settings_manager::SettingsScope::Global);let mut update=|current:Option<&str>|{let mut value:serde_json::Value=current.and_then(|text|serde_json::from_str(text).ok()).unwrap_or_default();value["memory"]["enabled"]=serde_json::json!(false);serde_json::to_string(&value).ok()};storage.with_lock(maho_core::settings_manager::SettingsScope::Global,&mut update).unwrap();let written:serde_json::Value=serde_json::from_slice(&std::fs::read(&settings_for_write).unwrap()).unwrap();assert_eq!(written["memory"]["enabled"],false);}).await.unwrap();
        owner.close().await.unwrap();assert!(!endpoint.exists());assert!(!agent.join("settings.jsonc.lock").exists());
        let path=settings.to_string_lossy().into_owned();assert!(tokio::task::spawn_blocking(move||maho_core::lockfile_policy::acquire_lock_sync(&path,1000)).await.unwrap().is_ok());
    }
}
