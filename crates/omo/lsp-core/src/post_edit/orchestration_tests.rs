use super::*;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

fn paths(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

fn block(file_path: &str, diagnostics: &str) -> PostEditDiagnosticsBlock {
    PostEditDiagnosticsBlock {
        file_path: file_path.to_string(),
        diagnostics: diagnostics.to_string(),
    }
}

fn text(value: &str) -> Result<PostEditDiagnosticsOutcome, DiagnosticsRunnerError> {
    Ok(PostEditDiagnosticsOutcome::Text(value.to_string()))
}

#[tokio::test]
async fn rendered_not_configured_text_is_not_cached() {
    let cache = create_post_edit_not_configured_cache();
    let calls = AtomicUsize::new(0);

    let first = collect_post_edit_diagnostics(CollectPostEditDiagnosticsInput {
        file_paths: &paths(&["a.foo"]),
        cache: Some(&cache),
        max_concurrency: None,
        run_diagnostics: |_| async {
            calls.fetch_add(1, Ordering::SeqCst);
            text("No LSP server configured for extension: .foo\n\nordinary text from a renderer")
        },
    })
    .await;
    let second = collect_post_edit_diagnostics(CollectPostEditDiagnosticsInput {
        file_paths: &paths(&["b.foo"]),
        cache: Some(&cache),
        max_concurrency: None,
        run_diagnostics: |_| async {
            calls.fetch_add(1, Ordering::SeqCst);
            text("diagnostic for b.foo")
        },
    })
    .await;

    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        first.blocks,
        vec![block(
            "a.foo",
            "No LSP server configured for extension: .foo\n\nordinary text from a renderer"
        )]
    );
    assert_eq!(second.blocks, vec![block("b.foo", "diagnostic for b.foo")]);
    assert_eq!(cache.not_configured_extensions(), Vec::<String>::new());
}

#[tokio::test]
async fn duplicate_paths_are_processed_once_and_blocks_stay_ordered() {
    let calls = Mutex::new(Vec::new());

    let result = collect_post_edit_diagnostics(CollectPostEditDiagnosticsInput {
        file_paths: &paths(&["a.ts", "b.ts", "a.ts", "c.ts"]),
        cache: None,
        max_concurrency: None,
        run_diagnostics: |file_path: String| {
            calls.lock().expect("calls").push(file_path.clone());
            async move {
                if file_path == "b.ts" {
                    text("No diagnostics found")
                } else {
                    text(&format!("diagnostic for {file_path}"))
                }
            }
        },
    })
    .await;

    assert_eq!(
        *calls.lock().expect("calls"),
        paths(&["a.ts", "b.ts", "c.ts"])
    );
    assert_eq!(
        result.blocks,
        vec![
            block("a.ts", "diagnostic for a.ts"),
            block("c.ts", "diagnostic for c.ts")
        ]
    );
}

#[tokio::test]
async fn concurrency_is_bounded_to_four_and_failures_isolate() {
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));

    let result = collect_post_edit_diagnostics(CollectPostEditDiagnosticsInput {
        file_paths: &paths(&["a.ts", "b.ts", "c.ts", "d.ts", "e.ts", "f.ts"]),
        cache: None,
        max_concurrency: None,
        run_diagnostics: |file_path: String| {
            let active = active.clone();
            let max_active = max_active.clone();
            async move {
                let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                max_active.fetch_max(now, Ordering::SeqCst);
                tokio::task::yield_now().await;
                active.fetch_sub(1, Ordering::SeqCst);
                match file_path.as_str() {
                    "c.ts" => Err(DiagnosticsRunnerError::new("server exploded")),
                    "e.ts" => text("No diagnostics found"),
                    _ => text(&format!("diagnostic for {file_path}")),
                }
            }
        },
    })
    .await;

    assert_eq!(max_active.load(Ordering::SeqCst), 4);
    assert_eq!(
        result.blocks,
        vec![
            block("a.ts", "diagnostic for a.ts"),
            block("b.ts", "diagnostic for b.ts"),
            block("c.ts", "server exploded"),
            block("d.ts", "diagnostic for d.ts"),
            block("f.ts", "diagnostic for f.ts"),
        ]
    );
}

#[tokio::test]
async fn only_structured_not_configured_is_cached_until_reset() {
    let cache = create_post_edit_not_configured_cache();
    let calls = Mutex::new(Vec::<String>::new());
    let responses = Mutex::new(HashMap::from([
        (
            "a.foo".to_string(),
            PostEditDiagnosticsOutcome::NotConfigured {
                extension: ".foo".to_string(),
            },
        ),
        (
            "b.foo".to_string(),
            PostEditDiagnosticsOutcome::Text("diagnostic should be skipped".to_string()),
        ),
        (
            "c.bar".to_string(),
            PostEditDiagnosticsOutcome::Text(
                "LSP server 'bar' for .bar is NOT INSTALLED.".to_string(),
            ),
        ),
    ]));
    let run = |file_path: String| {
        calls.lock().expect("calls").push(file_path.clone());
        let response = responses
            .lock()
            .expect("responses")
            .get(&file_path)
            .cloned()
            .unwrap_or_else(|| {
                PostEditDiagnosticsOutcome::Text("No diagnostics found".to_string())
            });
        async move { Ok(response) }
    };

    let first = collect_post_edit_diagnostics(CollectPostEditDiagnosticsInput {
        file_paths: &paths(&["a.foo", "c.bar"]),
        cache: Some(&cache),
        max_concurrency: None,
        run_diagnostics: run,
    })
    .await;
    assert_eq!(*calls.lock().expect("calls"), paths(&["a.foo", "c.bar"]));
    assert_eq!(
        first.blocks,
        vec![block(
            "c.bar",
            "LSP server 'bar' for .bar is NOT INSTALLED."
        )]
    );

    let cached = collect_post_edit_diagnostics(CollectPostEditDiagnosticsInput {
        file_paths: &paths(&["b.foo"]),
        cache: Some(&cache),
        max_concurrency: None,
        run_diagnostics: run,
    })
    .await;
    assert_eq!(cached.blocks, Vec::new());
    assert_eq!(*calls.lock().expect("calls"), paths(&["a.foo", "c.bar"]));

    responses.lock().expect("responses").insert(
        "a.foo".to_string(),
        PostEditDiagnosticsOutcome::Text("No diagnostics found".to_string()),
    );
    reset_post_edit_not_configured_cache(&cache);
    let second = collect_post_edit_diagnostics(CollectPostEditDiagnosticsInput {
        file_paths: &paths(&["a.foo", "b.foo"]),
        cache: Some(&cache),
        max_concurrency: None,
        run_diagnostics: run,
    })
    .await;

    assert_eq!(
        second.blocks,
        vec![block("b.foo", "diagnostic should be skipped")]
    );
    assert_eq!(
        *calls.lock().expect("calls"),
        paths(&["a.foo", "c.bar", "a.foo", "b.foo"])
    );
}

#[tokio::test]
async fn non_not_configured_failures_always_retry() {
    let cache = create_post_edit_not_configured_cache();
    let calls = AtomicUsize::new(0);

    let first = collect_post_edit_diagnostics(CollectPostEditDiagnosticsInput {
        file_paths: &paths(&["a.foo"]),
        cache: Some(&cache),
        max_concurrency: None,
        run_diagnostics: |_| async {
            calls.fetch_add(1, Ordering::SeqCst);
            text("LSP server 'foo' for .foo is NOT INSTALLED.")
        },
    })
    .await;
    let second = collect_post_edit_diagnostics(CollectPostEditDiagnosticsInput {
        file_paths: &paths(&["b.foo"]),
        cache: Some(&cache),
        max_concurrency: None,
        run_diagnostics: |_| async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(DiagnosticsRunnerError::new("daemon unavailable"))
        },
    })
    .await;

    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        first.blocks,
        vec![block(
            "a.foo",
            "LSP server 'foo' for .foo is NOT INSTALLED."
        )]
    );
    assert_eq!(second.blocks, vec![block("b.foo", "daemon unavailable")]);
    assert_eq!(cache.not_configured_extensions(), Vec::<String>::new());
}
