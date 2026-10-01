use maho_agent::{
    harness::{
        context::{background_context, with_abort_signal},
        env::nodejs::NodeExecutionEnv,
        tools::{tool_context::MutationTool, *},
        types::{AgentHarnessTool, AgentHarnessToolInvocation},
    },
    types::AgentToolResult,
};
use maho_ai::{
    types::{BoxFuture, ContentBlock},
    utils::abort::AbortController,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
struct Invocation;
impl AgentHarnessToolInvocation for Invocation {
    fn invocation_id(&self) -> &str {
        "post-mutate-result"
    }
    fn operation_id(&self) -> &str {
        "post-mutate-operation"
    }
    fn turn_id(&self) -> &str {
        "post-mutate-turn"
    }
    fn get_memo<'a>(&'a self, _: &'a str) -> BoxFuture<'a, Option<Value>> {
        Box::pin(async { None })
    }
    fn set_memo<'a>(&'a self, _: &'a str, _: Option<Value>) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}
struct Fixture {
    dir: PathBuf,
    context: ExecutionToolContext,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "t15b-hooks-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&dir).expect("tool test operation must succeed");
        Self {
            context: ExecutionToolContext {
                env: Arc::new(NodeExecutionEnv::new(dir.to_string_lossy())),
                post_mutate: None,
            },
            dir,
        }
    }
    fn put(&self, text: &str) {
        std::fs::write(self.dir.join("file.txt"), text).expect("tool test operation must succeed");
    }
    fn get(&self) -> String {
        std::fs::read_to_string(self.dir.join("file.txt")).expect("tool test operation must succeed")
    }
    async fn run(
        &self,
        tool: AgentHarnessTool<ExecutionToolContext>,
        input: Value,
    ) -> Result<AgentToolResult, String> {
        (tool.execute)(
            "call".into(),
            input,
            Arc::new(|_, _| {}),
            self.context.clone(),
            Arc::new(Invocation),
            background_context(),
        )
        .await
    }
    async fn write(&self, content: &str) -> Result<AgentToolResult, String> {
        self.run(
            create_write_tool(),
            json!({"path":"file.txt","content":content}),
        )
        .await
    }
    async fn edit(&self) -> Result<AgentToolResult, String> {
        self.run(
            create_edit_tool(),
            json!({"path":"file.txt","edits":[{"oldText":"alpha","newText":"ALPHA"}]}),
        )
        .await
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).expect("tool test operation must succeed");
    }
}
fn text(result: &AgentToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| match c {
            ContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
#[tokio::test]
async fn hook_receives_absolute_path_and_appends_write_note() {
    let mut f = Fixture::new();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = calls.clone();
    f.context.post_mutate = Some(Arc::new(move |input| {
        seen.lock().expect("tool test operation must succeed").push(input);
        Box::pin(async {
            Ok(PostMutateResult {
                changed: false,
                note: Some("formatted with biome".into()),
            })
        })
    }));
    let result = f
        .run(
            create_write_tool(),
            json!({"path":"nested/file.txt","content":"hello"}),
        )
        .await
        .expect("tool test operation must succeed");
    let calls = calls.lock().expect("tool test operation must succeed");
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].path,
        f.dir.join("nested/file.txt").to_string_lossy()
    );
    assert_eq!(calls[0].tool, MutationTool::Write);
    assert_eq!(
        text(&result),
        "Successfully wrote to nested/file.txt\nformatted with biome"
    );
}
#[tokio::test]
async fn hook_observes_freshly_written_bytes() {
    let mut f = Fixture::new();
    let seen = Arc::new(Mutex::new(None));
    let observed = seen.clone();
    f.context.post_mutate = Some(Arc::new(move |input| {
        *observed.lock().expect("tool test operation must succeed") = Some(std::fs::read_to_string(input.path).expect("tool test operation must succeed"));
        Box::pin(async { Ok(PostMutateResult::default()) })
    }));
    f.write("written-bytes").await.expect("tool test operation must succeed");
    assert_eq!(*seen.lock().expect("tool test operation must succeed"), Some("written-bytes".into()));
}
#[tokio::test]
async fn hook_receives_symlink_path_not_canonical_target() {
    let mut f = Fixture::new();
    f.put("alpha\nbeta\n");
    std::os::unix::fs::symlink("file.txt", f.dir.join("link.txt")).expect("tool test operation must succeed");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let observed = seen.clone();
    f.context.post_mutate = Some(Arc::new(move |input| {
        observed.lock().expect("tool test operation must succeed").push(input.path);
        Box::pin(async { Ok(PostMutateResult::default()) })
    }));
    f.run(
        create_edit_tool(),
        json!({"path":"link.txt","edits":[{"oldText":"alpha","newText":"ALPHA"}]}),
    )
    .await
    .expect("tool test operation must succeed");
    assert_eq!(
        *seen.lock().expect("tool test operation must succeed"),
        vec![f.dir.join("link.txt").to_string_lossy().into_owned()]
    );
    assert_eq!(f.get(), "ALPHA\nbeta\n");
}
#[tokio::test]
async fn edit_diff_is_recomputed_after_hook_rewrite() {
    let mut f = Fixture::new();
    f.put("alpha\nbeta\ngamma\n");
    f.context.post_mutate = Some(Arc::new(|input| {
        Box::pin(async move {
            let current = std::fs::read_to_string(&input.path).expect("tool test operation must succeed");
            std::fs::write(input.path, current.replace("gamma", "GAMMA")).expect("tool test operation must succeed");
            Ok(PostMutateResult {
                changed: true,
                note: Some("auto-formatted".into()),
            })
        })
    }));
    let result = f.edit().await.expect("tool test operation must succeed");
    assert_eq!(f.get(), "ALPHA\nbeta\nGAMMA\n");
    assert!(result.details["diff"].as_str().expect("tool test operation must succeed").contains("GAMMA"));
    assert!(
        result.details["patch"]
            .as_str()
            .expect("tool test operation must succeed")
            .contains("+GAMMA\n")
    );
    assert_eq!(
        text(&result),
        "Successfully replaced 1 block(s) in file.txt.\nauto-formatted"
    );
}
#[tokio::test]
async fn unchanged_hook_preserves_diff_metadata() {
    let mut f = Fixture::new();
    f.put("alpha\nbeta\n");
    f.context.post_mutate = Some(Arc::new(|_| {
        Box::pin(async { Ok(PostMutateResult::default()) })
    }));
    let result = f.edit().await.expect("tool test operation must succeed");
    assert_eq!(f.get(), "ALPHA\nbeta\n");
    assert_eq!(
        text(&result),
        "Successfully replaced 1 block(s) in file.txt."
    );
    assert_eq!(result.details["firstChangedLine"], 1);
}
#[tokio::test]
async fn hook_runs_inside_mutation_queue() {
    let mut f = Fixture::new();
    let events = Arc::new(Mutex::new(Vec::new()));
    let seen = events.clone();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let entered = Arc::new(Mutex::new(Some(entered_tx)));
    let release = Arc::new(Mutex::new(Some(release_rx)));
    f.context.post_mutate = Some(Arc::new(move |input| {
        let events = seen.clone();
        let entered = entered.clone();
        let release = release.clone();
        Box::pin(async move {
            let content = std::fs::read_to_string(input.path).expect("tool test operation must succeed");
            events.lock().expect("tool test operation must succeed").push(format!("hook-start:{content}"));
            if content == "first" {
                entered.lock().expect("tool test operation must succeed").take().expect("tool test operation must succeed").send(()).expect("tool test operation must succeed");
                let pending = release.lock().expect("tool test operation must succeed").take().expect("tool test operation must succeed");
                pending.await.expect("tool test operation must succeed");
            }
            events.lock().expect("tool test operation must succeed").push(format!("hook-end:{content}"));
            Ok(PostMutateResult::default())
        })
    }));
    let first = f.write("first");
    tokio::pin!(first);
    assert!(futures::poll!(&mut first).is_pending());
    tokio::time::timeout(std::time::Duration::from_secs(2), entered_rx)
        .await
        .expect("tool test operation must succeed")
        .expect("tool test operation must succeed");
    let second = f.write("second");
    tokio::pin!(second);
    assert!(futures::poll!(&mut second).is_pending());
    assert_eq!(f.get(), "first");
    release_tx.send(()).expect("tool test operation must succeed");
    let (a, b) = tokio::join!(first, second);
    a.expect("tool test operation must succeed");
    b.expect("tool test operation must succeed");
    assert_eq!(
        *events.lock().expect("tool test operation must succeed"),
        vec![
            "hook-start:first",
            "hook-end:first",
            "hook-start:second",
            "hook-end:second"
        ]
    );
    assert_eq!(f.get(), "second");
}
#[tokio::test]
async fn throwing_hook_keeps_landed_write() {
    let mut f = Fixture::new();
    f.context.post_mutate = Some(Arc::new(|_| {
        Box::pin(async { Err("formatter exploded".into()) })
    }));
    let result = f.write("payload").await.expect("tool test operation must succeed");
    assert_eq!(f.get(), "payload");
    assert_eq!(
        text(&result),
        "Successfully wrote to file.txt\npostMutate hook failed: formatter exploded"
    );
}
#[tokio::test]
async fn throwing_hook_keeps_landed_edit() {
    let mut f = Fixture::new();
    f.put("alpha\nbeta\n");
    f.context.post_mutate = Some(Arc::new(|_| {
        Box::pin(async { Err("formatter exploded".into()) })
    }));
    let result = f.edit().await.expect("tool test operation must succeed");
    assert_eq!(f.get(), "ALPHA\nbeta\n");
    assert_eq!(
        text(&result),
        "Successfully replaced 1 block(s) in file.txt.\npostMutate hook failed: formatter exploded"
    );
    assert!(result.details["diff"].as_str().expect("tool test operation must succeed").contains("ALPHA"));
}
#[tokio::test]
async fn partial_hook_rewrite_is_reflected_after_error() {
    let mut f = Fixture::new();
    f.put("alpha\nbeta\ngamma\n");
    f.context.post_mutate = Some(Arc::new(|input| {
        Box::pin(async move {
            let current = std::fs::read_to_string(&input.path).expect("tool test operation must succeed");
            std::fs::write(input.path, current.replace("gamma", "GAMMA")).expect("tool test operation must succeed");
            Err("formatter exploded after writing".into())
        })
    }));
    let result = f.edit().await.expect("tool test operation must succeed");
    assert_eq!(f.get(), "ALPHA\nbeta\nGAMMA\n");
    assert!(
        result.details["patch"]
            .as_str()
            .expect("tool test operation must succeed")
            .contains("+GAMMA\n")
    );
    assert!(text(&result).ends_with("postMutate hook failed: formatter exploded after writing"));
}
#[tokio::test]
async fn reread_failure_keeps_landed_edit_result() {
    let mut f = Fixture::new();
    f.put("alpha\nbeta\n");
    f.context.post_mutate = Some(Arc::new(|input| {
        Box::pin(async move {
            std::fs::remove_file(input.path).expect("tool test operation must succeed");
            Ok(PostMutateResult {
                changed: true,
                note: Some("replaced the file with nothing".into()),
            })
        })
    }));
    let result = f.edit().await.expect("tool test operation must succeed");
    assert_eq!(
        text(&result),
        "Successfully replaced 1 block(s) in file.txt.\nreplaced the file with nothing\npostMutate left the file unreadable: not_found. Reported diff describes the edit before the hook ran."
    );
    assert!(result.details["diff"].as_str().expect("tool test operation must succeed").contains("ALPHA"));
}
#[tokio::test]
async fn hook_receives_abort_signal_and_aborts_result() {
    let mut f = Fixture::new();
    let controller = AbortController::new();
    let abort = controller.clone();
    let seen = Arc::new(Mutex::new(None));
    let observed = seen.clone();
    f.context.post_mutate = Some(Arc::new(move |input| {
        let abort = abort.clone();
        *observed.lock().expect("tool test operation must succeed") = input.signal;
        Box::pin(async move {
            abort.abort(None);
            Ok(PostMutateResult::default())
        })
    }));
    let result = (create_write_tool().execute)(
        "call".into(),
        json!({"path":"file.txt","content":"payload"}),
        Arc::new(|_, _| {}),
        f.context.clone(),
        Arc::new(Invocation),
        with_abort_signal(controller.signal(), &background_context()),
    )
    .await;
    assert_eq!(result.unwrap_err(), "Operation aborted");
    assert!(seen.lock().expect("tool test operation must succeed").as_ref().expect("tool test operation must succeed").aborted());
    assert_eq!(f.get(), "payload");
}
#[tokio::test]
async fn preaborted_edit_skips_hook() {
    let mut f = Fixture::new();
    let controller = AbortController::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    f.context.post_mutate = Some(Arc::new(move |_| {
        seen.fetch_add(1, Ordering::Relaxed);
        Box::pin(async { Ok(PostMutateResult::default()) })
    }));
    controller.abort(None);
    let result = (create_edit_tool().execute)(
        "call".into(),
        json!({"path":"missing.txt","edits":[{"oldText":"a","newText":"b"}]}),
        Arc::new(|_, _| {}),
        f.context.clone(),
        Arc::new(Invocation),
        with_abort_signal(controller.signal(), &background_context()),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}
#[tokio::test]
async fn absent_hook_preserves_original_results() {
    let f = Fixture::new();
    let write = f.write("hello").await.expect("tool test operation must succeed");
    assert_eq!(text(&write), "Successfully wrote to file.txt");
    assert!(write.details.is_null());
    f.put("alpha\nbeta\n");
    let edit = f.edit().await.expect("tool test operation must succeed");
    assert_eq!(text(&edit), "Successfully replaced 1 block(s) in file.txt.");
    assert_eq!(edit.details["firstChangedLine"], 1);
    assert_eq!(f.get(), "ALPHA\nbeta\n");
}
