use std::{collections::HashMap, future::Future, path::{Path, PathBuf}, sync::{Arc, OnceLock, Weak}};
use tokio::sync::{Mutex, OwnedMutexGuard};
use crate::{definition::ToolError, filesystem_policy::canonicalize_filesystem_path};
type QueueMap = HashMap<PathBuf, Weak<Mutex<()>>>;
static QUEUES: OnceLock<Mutex<QueueMap>> = OnceLock::new();
pub async fn lock_file_mutation(path: &Path) -> Result<OwnedMutexGuard<()>, ToolError> {
    let mut queues = QUEUES.get_or_init(|| Mutex::new(HashMap::new())).lock().await;
    let key = crate::bounded_realpath::fold_path_for_case_insensitive_filesystem(canonicalize_filesystem_path(path).await?);
    queues.retain(|_, queue| queue.strong_count() > 0);
    let queue = queues.get(&key).and_then(Weak::upgrade).unwrap_or_else(|| {
        let queue = Arc::new(Mutex::new(()));
        queues.insert(key, Arc::downgrade(&queue));
        queue
    });
    // Register the waiter before releasing registration order.
    let waiting = queue.lock_owned();
    tokio::pin!(waiting);
    let guard = std::future::poll_fn(|cx| match waiting.as_mut().poll(cx) {
        std::task::Poll::Ready(guard) => std::task::Poll::Ready(Some(guard)),
        std::task::Poll::Pending => std::task::Poll::Ready(None),
    }).await;
    drop(queues);
    match guard { Some(guard) => Ok(guard), None => Ok(waiting.await) }
}
