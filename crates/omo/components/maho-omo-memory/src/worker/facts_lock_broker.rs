#[cfg(unix)]
use std::{path::{Path, PathBuf}, sync::Arc};
#[cfg(unix)]
use tokio::{io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader}, net::{UnixListener, UnixStream}, sync::watch, task::JoinSet};

#[cfg(unix)]
pub struct FactsLockBroker {
    listener: UnixListener,
    socket_path: PathBuf,
    auth_path: PathBuf,
    settings_path: PathBuf,
    owned_socket_dir: PathBuf,
    #[cfg(test)]
    acquire_hook: std::sync::Mutex<Option<(tokio::sync::mpsc::UnboundedSender<()>,tokio::sync::oneshot::Receiver<()>)>>,
    #[cfg(test)]
    write_hook: std::sync::Mutex<Option<(tokio::sync::mpsc::UnboundedSender<()>,tokio::sync::oneshot::Receiver<()>)>>,
}
#[cfg(unix)]
pub struct FactsLockBrokerTask {
    shutdown: Option<watch::Sender<bool>>,
    task: Option<tokio::task::JoinHandle<Result<(), String>>>,
    broker: Option<Arc<FactsLockBroker>>,
}
#[cfg(unix)]
impl FactsLockBrokerTask {
    pub fn start(broker: Arc<FactsLockBroker>) -> Self {
        let (shutdown, receiver) = watch::channel(false);
        let task = tokio::spawn(broker.clone().serve(receiver));
        Self { shutdown: Some(shutdown), task: Some(task), broker: Some(broker) }
    }
    pub async fn close(&mut self) -> Result<(), String> {
        if let Some(shutdown) = &self.shutdown { let _ = shutdown.send(true); }
        if let Some(task) = &mut self.task {
            let result = (&mut *task).await.map_err(|error| error.to_string())?;
            result?;
            self.task.take();
        }
        self.shutdown.take();
        self.broker.take();
        Ok(())
    }
}
#[cfg(unix)]
impl Drop for FactsLockBrokerTask {
    fn drop(&mut self) {
        if let Some(shutdown) = &self.shutdown { let _ = shutdown.send(true); }
    }
}
#[cfg(unix)]
impl Drop for FactsLockBroker {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.socket_path) && error.kind() != std::io::ErrorKind::NotFound { maho_ext_api::diagnostic(&format!("facts broker socket cleanup failed: {error}")); }
        if let Err(error)=std::fs::remove_dir(&self.owned_socket_dir) && error.kind()!=std::io::ErrorKind::NotFound {maho_ext_api::diagnostic(&format!("facts broker directory cleanup failed: {error}"));}
    }
}

#[cfg(unix)]
impl FactsLockBroker {
    pub fn bind(auth_path: &Path, settings_path: &Path) -> Result<Self, String> {
        let mut owned_socket_dir=std::env::temp_dir().join(format!("facts-lock-{}",memory_core::support::random::random_uuid()));
        if owned_socket_dir.join("broker.sock").as_os_str().as_encoded_bytes().len() > 100 {
            owned_socket_dir=PathBuf::from("/tmp").join(format!("facts-lock-{}",memory_core::support::random::random_uuid()));
        }
        let mut dir_builder=std::fs::DirBuilder::new();
        use std::os::unix::fs::DirBuilderExt;dir_builder.mode(0o700);dir_builder.create(&owned_socket_dir).map_err(|error|error.to_string())?;
        let socket_dir=owned_socket_dir.clone();
        let socket_path = socket_dir.join("broker.sock");
        let listener = match UnixListener::bind(&socket_path) {
            Ok(listener) => listener,
            Err(error) => {return match std::fs::remove_dir(&owned_socket_dir){Ok(())=>Err(error.to_string()),Err(cleanup)if cleanup.kind()==std::io::ErrorKind::NotFound=>Err(error.to_string()),Err(cleanup)=>Err(format!("{error}; broker directory cleanup failed: {cleanup}"))};},
        };
        Ok(Self { listener, socket_path, auth_path: auth_path.to_owned(), settings_path: settings_path.to_owned(), owned_socket_dir, #[cfg(test)] acquire_hook:std::sync::Mutex::new(None), #[cfg(test)] write_hook:std::sync::Mutex::new(None) })
    }

    #[cfg(test)]
    pub fn with_acquire_gate(self, entered:tokio::sync::mpsc::UnboundedSender<()>,release:tokio::sync::oneshot::Receiver<()>) -> Self { *self.acquire_hook.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=Some((entered,release));self }
    #[cfg(test)]
    pub fn with_write_gate(self, entered:tokio::sync::mpsc::UnboundedSender<()>,release:tokio::sync::oneshot::Receiver<()>) -> Self { *self.write_hook.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=Some((entered,release));self }

    pub fn endpoint(&self) -> &Path { &self.socket_path }

    pub async fn serve(self: Arc<Self>, mut shutdown: watch::Receiver<bool>) -> Result<(), String> {
        let mut clients = JoinSet::new();
        let result = loop {
            tokio::select! {
                joined = clients.join_next(), if !clients.is_empty() => {
                    if let Some(result) = joined {
                        match result {
                            Ok(Ok(())) => {},
                            Ok(Err(error)) => maho_ext_api::diagnostic(&format!("facts lock broker client failed: {error}")),
                            Err(error) => break Err(error.to_string()),
                        }
                    }
                }
                accepted = self.listener.accept() => {
                    if *shutdown.borrow() { break Ok(()); }
                    let (stream, _) = match accepted { Ok(accepted) => accepted, Err(error) => break Err(error.to_string()) };
                    let broker = self.clone();
                    let client_shutdown=shutdown.clone();
                    clients.spawn(async move { broker.serve_client(stream,client_shutdown).await });
                }
                changed = shutdown.changed() => if changed.is_err() || *shutdown.borrow() { break Ok(()); },
            }
        };
        while clients.join_next().await.is_some() {}
        result
    }

    async fn serve_client(&self, stream: UnixStream, mut shutdown: watch::Receiver<bool>) -> Result<(), String> {
        if *shutdown.borrow() { return Ok(()); }
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        let mut frame = Vec::new();
        let mut bounded_read=(&mut read).take(16*1024);
        let read_request = tokio::time::timeout(std::time::Duration::from_secs(10),bounded_read.read_until(b'\n',&mut frame));
        tokio::pin!(read_request);
        let count = tokio::select! { _=shutdown.changed()=>return Ok(()), result=&mut read_request=>result.map_err(|_|"facts lock request timed out".to_owned())?.map_err(|error|error.to_string())? };
        if count == 0 || frame.last() != Some(&b'\n') { return Err("invalid facts lock request frame".into()); }
        let request: BrokerRequest = serde_json::from_slice(&frame).map_err(|error| error.to_string())?;
        let acquired_key = request.acquire;
        let path = match acquired_key.as_str() {
            "auth" => &self.auth_path,
            "settings" => &self.settings_path,
            _ => return Self::respond(&mut write, Err("unknown facts lock key".into())).await,
        };
        let key_path = path.to_string_lossy().into_owned();
        let budget = request.budget_ms.min(5_500);
        #[cfg(test)]
        let acquire_hook=self.acquire_hook.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        #[cfg(test)]
        let acquire=tokio::task::spawn_blocking(move || {
            if let Some((entered,release))=acquire_hook { let _=entered.send(());let _=release.blocking_recv(); }
            maho_core::lockfile_policy::acquire_host_lock_sync(&key_path,budget)
        });
        #[cfg(not(test))]
        let acquire=tokio::task::spawn_blocking(move || maho_core::lockfile_policy::acquire_host_lock_sync(&key_path,budget));
        tokio::pin!(acquire);
        let guard=tokio::select! {
            _=shutdown.changed()=> {
                match acquire.await {
                    Ok(Ok(guard))=>drop(guard),
                    Ok(Err(_))=>{},
                    Err(_)=>{},
                }
                return Ok(());
            },
            result=&mut acquire=>result.map_err(|error|error.to_string())?,
        };
        let mut guard = match guard {
            Ok(guard) => { Self::respond(&mut write, Ok(())).await?; guard }
            Err(error) => { Self::respond(&mut write, Err(error.message())).await?; return Ok(()); }
        };
        loop {
            let mut frame = Vec::new();
            let mut bounded_read=(&mut read).take(16*1024*1024);
            let read_request = tokio::time::timeout(std::time::Duration::from_secs(15*60),bounded_read.read_until(b'\n',&mut frame));
            tokio::pin!(read_request);
            let count = tokio::select! { _=shutdown.changed()=>return Ok(()),result=&mut read_request=>result.map_err(|_|"facts lock lease timed out".to_owned())?.map_err(|error|error.to_string())? };
            if count == 0 { return Ok(()); }
            if frame.last() != Some(&b'\n') { return Err("invalid facts lock write frame".into()); }
            let request: BrokerWrite = serde_json::from_slice(&frame).map_err(|error| error.to_string())?;
            if request.key != acquired_key { return Self::respond(&mut write, Err("facts lock key changed during lease".into())).await; }
            let path = path.to_owned();
            let content = request.write;
            let auth = request.key == "auth";
            #[cfg(test)]
            let write_hook=self.write_hook.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
            let (returned_guard, result) = tokio::task::spawn_blocking(move || {
                #[cfg(test)]
                if let Some((entered, release)) = write_hook { let _=entered.send(()); let _=release.blocking_recv(); }
                (guard, write_atomic(&path, &content, auth))
            })
                .await.map_err(|error| error.to_string())?;
            guard = returned_guard;
            Self::respond(&mut write, result).await?;
        }
    }

    async fn respond(write: &mut tokio::net::unix::OwnedWriteHalf, result: Result<(), String>) -> Result<(), String> {
        let response = serde_json::json!({"result": result});
        tokio::time::timeout(std::time::Duration::from_secs(5), write.write_all(format!("{response}\n").as_bytes()))
            .await.map_err(|_| "facts lock response timed out".to_owned())?.map_err(|error| error.to_string())
    }
}

#[cfg(unix)]
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BrokerRequest { acquire: String, budget_ms: u64 }

#[cfg(unix)]
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BrokerWrite { key: String, write: String }

#[cfg(unix)]
fn write_atomic(path: &Path, content: &str, auth: bool) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path.parent().ok_or("broker target has no parent")?;
    let file = path.file_name().ok_or("broker target has no filename")?.to_string_lossy();
    let temporary = parent.join(format!(".{file}.{}-{}-broker.tmp", std::process::id(), memory_core::support::random::random_uuid()));
    let mode = if auth { 0o600 } else { std::fs::metadata(path).map(|metadata| {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o777
    }).unwrap_or(0o600) };
    let mut created = false;
    let result = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(&temporary).map_err(|error| error.to_string())?;
        created = true;
        file.write_all(content.as_bytes()).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        std::fs::rename(&temporary, path).map_err(|error| error.to_string())
    })();
    if let Err(reason) = result {
        if !created { return Err(reason); }
        match std::fs::remove_file(&temporary) {
            Ok(()) => {},
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
            Err(error) => return Err(format!("{reason}; temporary cleanup failed: {error}")),
        }
        return Err(reason);
    }
    result
}
