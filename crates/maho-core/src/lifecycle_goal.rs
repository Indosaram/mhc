//! The no-goal fast path performs no asynchronous filesystem operation.
use std::path::Path;
pub fn read_lifecycle_goal<T, F, Fut>(
    path: &Path,
    read: F,
) -> impl std::future::Future<Output = std::io::Result<Option<T>>>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<Option<T>>>,
{
    let exists = match std::fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    };
    async move { if exists? { read().await } else { Ok(None) } }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn absent_goal_never_starts_reader() {
        let root = tempfile::tempdir().expect("temp");
        let result = read_lifecycle_goal::<(), _, _>(&root.path().join("missing"), || async {
            panic!("unrelated async IO started")
        })
        .await
        .expect("absence");
        assert!(result.is_none());
    }
}
