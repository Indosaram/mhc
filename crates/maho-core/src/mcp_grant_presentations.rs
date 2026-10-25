//! Session-scoped grant UI registration and fail-closed bounded selection.
use maho_ai::{
    types::BoxFuture,
    utils::abort::{AbortController, AbortSignal},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

pub type Select = Arc<
    dyn Fn(String, Vec<String>, AbortSignal) -> BoxFuture<'static, Result<Option<String>, String>>
        + Send
        + Sync,
>;
#[derive(Clone)]
pub struct Presentation {
    select: Select,
    abort: AbortController,
}
type Presentations = BTreeMap<String, Vec<(u64, Presentation)>>;
static PRESENTATIONS: OnceLock<Mutex<Presentations>> = OnceLock::new();
static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
fn presentations() -> &'static Mutex<Presentations> {
    PRESENTATIONS.get_or_init(Mutex::default)
}
pub struct McpGrantAttachment {
    session_id: String,
    id: u64,
    abort: AbortController,
}
impl Drop for McpGrantAttachment {
    fn drop(&mut self) {
        self.abort.abort(None);
        let mut map = presentations()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entries) = map.get_mut(&self.session_id) {
            entries.retain(|(id, _)| *id != self.id);
            if entries.is_empty() {
                map.remove(&self.session_id);
            }
        }
    }
}
pub fn attach_mcp_grant_presentation(session_id: &str, select: Select) -> McpGrantAttachment {
    let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let abort = AbortController::new();
    presentations()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .entry(session_id.into())
        .or_default()
        .push((
            id,
            Presentation {
                select,
                abort: abort.clone(),
            },
        ));
    McpGrantAttachment {
        session_id: session_id.into(),
        id,
        abort,
    }
}
pub fn get_mcp_grant_presentation(session_id: Option<&str>) -> Option<Presentation> {
    presentations()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(session_id?)?
        .first()
        .map(|(_, entry)| entry.clone())
}
pub fn clear_mcp_grant_presentations(session_id: &str) {
    if let Some(entries) = presentations()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(session_id)
    {
        for (_, entry) in entries {
            entry.abort.abort(None);
        }
    }
}
pub async fn select_mcp_grant(
    select: Option<Presentation>,
    title: String,
    signal: Option<AbortSignal>,
) -> bool {
    let Some(entry) = select else { return false };
    if entry.abort.signal().aborted() || signal.as_ref().is_some_and(AbortSignal::aborted) {
        return false;
    }
    let abort = AbortController::new();
    let external = async {
        match &signal {
            Some(signal) => signal.cancelled().await,
            None => std::future::pending().await,
        }
    };
    let detached = entry.abort.signal();
    let decision = (entry.select)(
        title,
        vec![
            "Grant and save permanently".into(),
            "Start without them".into(),
        ],
        abort.signal(),
    );
    let granted = tokio::select! {
        biased;
        () = external => false,
        () = detached.cancelled() => false,
        result = tokio::time::timeout(Duration::from_secs(120), decision) => matches!(result, Ok(Ok(Some(answer))) if answer == "Grant and save permanently"),
    };
    abort.abort(None);
    granted
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn missing_or_disconnected_ui_denies() {
        assert!(!select_mcp_grant(None, String::new(), None).await);
        let attachment = attach_mcp_grant_presentation(
            "grant-test",
            Arc::new(|_, _, _| Box::pin(std::future::pending())),
        );
        let presentation = get_mcp_grant_presentation(Some("grant-test"));
        drop(attachment);
        assert!(!select_mcp_grant(presentation, String::new(), None).await);
    }
    #[tokio::test]
    async fn grant_is_explicit() {
        let attachment = attach_mcp_grant_presentation(
            "grant-yes",
            Arc::new(|_, _, _| Box::pin(async { Ok(Some("Grant and save permanently".into())) })),
        );
        assert!(
            select_mcp_grant(
                get_mcp_grant_presentation(Some("grant-yes")),
                String::new(),
                None
            )
            .await
        );
        drop(attachment);
    }
}
