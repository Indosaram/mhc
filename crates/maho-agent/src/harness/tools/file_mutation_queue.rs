use crate::harness::{
    context::Context,
    types::{ExecutionEnv, FileErrorCode},
};
use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, LazyLock, Mutex, Weak},
};
use tokio::sync::Mutex as AsyncMutex;
#[derive(Default)]
struct MutationQueueState {
    registration: AsyncMutex<HashMap<String, Weak<AsyncMutex<()>>>>,
}
struct Entry {
    env: Weak<dyn ExecutionEnv>,
    state: Arc<MutationQueueState>,
}
static STATES: LazyLock<Mutex<Vec<Entry>>> = LazyLock::new(|| Mutex::new(Vec::new()));
fn get_state(env: &Arc<dyn ExecutionEnv>) -> Arc<MutationQueueState> {
    let mut states = STATES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    states.retain(|entry| entry.env.strong_count() > 0);
    if let Some(entry) = states.iter().find(|entry| {
        entry
            .env
            .upgrade()
            .is_some_and(|existing| Arc::ptr_eq(&existing, env))
    }) {
        return entry.state.clone();
    }
    let state = Arc::new(MutationQueueState::default());
    states.push(Entry {
        env: Arc::downgrade(env),
        state: state.clone(),
    });
    state
}
pub async fn with_file_mutation_queue<T, F: Future<Output = Result<T, String>>>(
    env: &Arc<dyn ExecutionEnv>,
    path: &str,
    action: impl FnOnce() -> F,
    context: &Context,
) -> Result<T, String> {
    let state = get_state(env);
    let mut queues = state.registration.lock().await;
    let absolute = env
        .absolute_path(path, context)
        .await
        .map_err(|e| e.to_string())?;
    let key = match env.canonical_path(&absolute, context).await {
        Ok(path) => path,
        Err(error) if error.code == FileErrorCode::NotFound => {
            let mut ancestor = std::path::PathBuf::from(&absolute);
            let mut missing = Vec::new();
            let canonical_ancestor = loop {
                let Some(name) = ancestor.file_name() else {
                    return Err(error.to_string());
                };
                missing.push(name.to_os_string());
                if !ancestor.pop() {
                    return Err(error.to_string());
                }
                let ancestor_path = ancestor.to_string_lossy().into_owned();
                match env.canonical_path(&ancestor_path, context).await {
                    Ok(path) => break path,
                    Err(ancestor_error) if ancestor_error.code == FileErrorCode::NotFound => {}
                    Err(ancestor_error) if ancestor_error.code == FileErrorCode::NotSupported => {
                        missing.clear();
                        break absolute.clone();
                    }
                    Err(ancestor_error) => return Err(ancestor_error.to_string()),
                }
            };
            missing.reverse();
            missing.iter().fold(canonical_ancestor, |path, component| {
                std::path::Path::new(&path)
                    .join(component)
                    .to_string_lossy()
                    .into_owned()
            })
        }
        Err(error) if error.code == FileErrorCode::NotSupported => absolute,
        Err(error) => return Err(error.to_string()),
    };
    queues.retain(|_, queue| queue.strong_count() > 0);
    let queue = queues
        .get(&key)
        .and_then(Weak::upgrade)
        .unwrap_or_else(|| Arc::new(AsyncMutex::new(())));
    queues.insert(key, Arc::downgrade(&queue));
    let pending = queue.lock();
    tokio::pin!(pending);
    let guard = match futures::poll!(&mut pending) {
        std::task::Poll::Ready(guard) => {
            drop(queues);
            guard
        }
        std::task::Poll::Pending => {
            drop(queues);
            pending.await
        }
    };
    let result = action().await;
    drop(guard);
    result
}
