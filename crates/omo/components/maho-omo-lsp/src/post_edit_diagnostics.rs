use lsp_core::post_edit::{
    CollectPostEditDiagnosticsInput, DiagnosticsRunnerError, PostEditDiagnosticsOutcome,
    PostEditNotConfiguredCache, collect_post_edit_diagnostics,
    reset_post_edit_not_configured_cache,
};
use maho_ext_api::{ToolContent, ToolResultEvent};
use serde_json::Value;
use std::{collections::HashMap, future::Future, sync::Arc};

pub const POST_EDIT_DIAGNOSTICS_WIDGET_KEY: &str = "omo-senpi-lsp";
#[derive(Default)]
pub struct LspPostEditSessionState {
    caches: HashMap<String, Arc<PostEditNotConfiguredCache>>,
}
impl LspPostEditSessionState {
    pub fn get_or_create(&mut self, id: Option<&str>) -> Arc<PostEditNotConfiguredCache> {
        match id {
            Some(id) => Arc::clone(self.caches.entry(id.into()).or_default()),
            None => Arc::default(),
        }
    }
    pub fn on_session_start(&mut self, id: Option<&str>) {
        if let Some(id) = id {
            self.caches.entry(id.into()).or_default();
        }
    }
    pub fn reset(&mut self, id: Option<&str>) {
        if let Some(id) = id {
            reset_post_edit_not_configured_cache(&self.get_or_create(Some(id)));
        }
    }
    pub fn delete(&mut self, id: Option<&str>) {
        if let Some(id) = id {
            self.caches.remove(id);
        }
    }
}
pub fn should_run_post_edit_diagnostics(event: &ToolResultEvent) -> bool {
    !event.is_error && matches!(event.tool_name.as_str(), "write" | "edit" | "apply_patch")
}
pub struct PostEditDiagnosticsResult {
    pub content: Option<Vec<ToolContent>>,
    pub widget_lines: Option<Vec<String>>,
}
pub async fn append_post_edit_diagnostics<F, Fut>(
    event: &ToolResultEvent,
    run: F,
    cache: Option<&PostEditNotConfiguredCache>,
) -> Option<PostEditDiagnosticsResult>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<PostEditDiagnosticsOutcome, DiagnosticsRunnerError>>,
{
    if !should_run_post_edit_diagnostics(event) {
        return None;
    }
    let paths = extract_mutated_file_paths(event);
    if paths.is_empty() {
        return None;
    }
    let result = collect_post_edit_diagnostics(CollectPostEditDiagnosticsInput {
        file_paths: &paths,
        run_diagnostics: run,
        cache,
        max_concurrency: Some(4),
    })
    .await;
    let content = if result.blocks.is_empty() {
        None
    } else {
        let mut content = event.content.clone();
        content.extend(result.blocks.into_iter().map(|b| {
            ToolContent::text(format!(
                "\n\nLSP errors detected in {}, please fix:\n{}",
                b.file_path, b.diagnostics
            ))
        }));
        Some(content)
    };
    Some(PostEditDiagnosticsResult {
        content,
        widget_lines: None,
    })
}
pub fn extract_mutated_file_paths(event: &ToolResultEvent) -> Vec<String> {
    let mut paths = Vec::new();
    let input = &event.input;
    for key in ["path", "filePath"] {
        add(&mut paths, input.get(key));
    }
    for key in ["paths", "filePaths"] {
        if let Some(items) = input.get(key).and_then(Value::as_array) {
            for item in items {
                add(&mut paths, Some(item));
            }
        }
    }
    if let Some(patch) = input.get("input").and_then(Value::as_str) {
        for line in patch.split('\n') {
            for prefix in ["*** Add File: ", "*** Update File: ", "*** Move to: "] {
                if let Some(path) = line.strip_prefix(prefix) {
                    let path = path.trim().to_owned();
                    if !paths.contains(&path) {
                        paths.push(path);
                    }
                }
            }
        }
    }
    for key in ["files", "changes"] {
        if let Some(items) = input.get(key).and_then(Value::as_array) {
            for item in items {
                for key in ["path", "filePath", "movePath"] {
                    add(&mut paths, item.get(key));
                }
            }
        }
    }
    paths
}
fn add(paths: &mut Vec<String>, value: Option<&Value>) {
    if let Some(path) = value.and_then(Value::as_str).filter(|s| !s.is_empty())
        && !paths.iter().any(|p| p == path)
    {
        paths.push(path.into());
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn event(tool: &str, input: Value, error: bool) -> ToolResultEvent {
        ToolResultEvent {
            tool_call_id: "call".into(),
            tool_name: tool.into(),
            input,
            content: vec![ToolContent::text("ok")],
            details: None,
            is_error: error,
            usage: None,
        }
    }
    #[tokio::test]
    async fn injects_errors() {
        let e = event("write", json!({"path":"a.ts"}), false);
        let r = append_post_edit_diagnostics(
            &e,
            |_| async { Ok(PostEditDiagnosticsOutcome::Text("error".into())) },
            None,
        )
        .await
        .unwrap();
        assert_eq!(r.content.unwrap().len(), 2);
        assert!(r.widget_lines.is_none());
    }
    #[tokio::test]
    async fn clean_no_injection() {
        let e = event("write", json!({"path":"a.ts"}), false);
        let r = append_post_edit_diagnostics(
            &e,
            |_| async {
                Ok(PostEditDiagnosticsOutcome::Text(
                    "No diagnostics found".into(),
                ))
            },
            None,
        )
        .await
        .unwrap();
        assert!(r.content.is_none());
    }
    #[tokio::test]
    async fn non_mutation_skipped() {
        let e = event("bash", json!({"path":"a.ts"}), false);
        assert!(
            append_post_edit_diagnostics(
                &e,
                |_| async {
                    panic!("must not run");
                },
                None
            )
            .await
            .is_none()
        );
    }
    #[tokio::test]
    async fn failed_mutation_skipped() {
        let e = event("edit", json!({"path":"a.ts"}), true);
        assert!(
            append_post_edit_diagnostics(
                &e,
                |_| async {
                    panic!("must not run");
                },
                None
            )
            .await
            .is_none()
        );
    }
    #[test]
    fn extracts_patch_and_arrays() {
        let e = event(
            "apply_patch",
            json!({"path":"a","filePath":"a","paths":["b",null,""],"filePaths":["c"],"input":"*** Update File: d\n*** Move to: e\n*** Delete File: f","files":[{"path":"g","filePath":"h","movePath":"i"}],"changes":[{"path":"j"}]}),
            false,
        );
        assert_eq!(
            extract_mutated_file_paths(&e),
            ["a", "b", "c", "d", "e", "g", "h", "i", "j"]
        );
    }
    #[tokio::test]
    async fn session_cache_reset_delete() {
        let mut s = LspPostEditSessionState::default();
        s.on_session_start(Some("parent"));
        let p = s.get_or_create(Some("parent"));
        let c = s.get_or_create(Some("child"));
        let e = event("write", json!({"path":"a.foo"}), false);
        append_post_edit_diagnostics(
            &e,
            |_| async {
                Ok(PostEditDiagnosticsOutcome::NotConfigured {
                    extension: ".foo".into(),
                })
            },
            Some(&p),
        )
        .await;
        s.on_session_start(Some("parent"));
        assert_eq!(
            s.get_or_create(Some("parent")).not_configured_extensions(),
            vec![".foo"]
        );
        assert!(c.not_configured_extensions().is_empty());
        s.reset(Some("parent"));
        assert!(p.not_configured_extensions().is_empty());
        s.delete(Some("parent"));
        assert!(!Arc::ptr_eq(&p, &s.get_or_create(Some("parent"))));
    }
    #[tokio::test]
    async fn failures_isolated_order_preserved() {
        let e = event("edit", json!({"filePaths":["a","b","a","c"]}), false);
        let r = append_post_edit_diagnostics(
            &e,
            |p| async move {
                if p == "b" {
                    Err(DiagnosticsRunnerError::new("exploded"))
                } else {
                    Ok(PostEditDiagnosticsOutcome::Text(p))
                }
            },
            None,
        )
        .await
        .unwrap();
        let text = r
            .content
            .unwrap()
            .into_iter()
            .filter_map(|c| match c {
                ToolContent::Text { text, .. } => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(text.len(), 4);
        assert!(text[1].ends_with("\na"));
        assert!(text[2].ends_with("\nexploded"));
        assert!(text[3].ends_with("\nc"));
    }
    #[tokio::test]
    async fn diagnostics_concurrency_is_four() {
        use std::sync::atomic::{AtomicUsize,Ordering};
        let active=Arc::new(AtomicUsize::new(0));let peak=Arc::new(AtomicUsize::new(0));
        let e=event("edit",json!({"filePaths":["a","b","a","c","d","e","f"]}),false);
        let r=append_post_edit_diagnostics(&e,|p| {let active=Arc::clone(&active);let peak=Arc::clone(&peak);async move {let n=active.fetch_add(1,Ordering::SeqCst)+1;peak.fetch_max(n,Ordering::SeqCst);let mut first=true;std::future::poll_fn(|cx| {if first {first=false;cx.waker().wake_by_ref();std::task::Poll::Pending} else {std::task::Poll::Ready(())}}).await;active.fetch_sub(1,Ordering::SeqCst);if p=="c" {Err(DiagnosticsRunnerError::new("server exploded"))} else {Ok(PostEditDiagnosticsOutcome::Text(if p=="e" {"No diagnostics found".into()} else {p}))}}},None).await.unwrap();
        assert_eq!(peak.load(Ordering::SeqCst),4);assert_eq!(active.load(Ordering::SeqCst),0);assert_eq!(r.content.unwrap().len(),6);
    }
}
