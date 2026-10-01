use maho_interactive::compaction_queue_transfer::*;
use maho_ext_api::StreamingBehavior;

#[derive(Default)]
struct QueueHost {
    batch: Vec<CompactionQueuedMessage>,
    restored: Vec<String>,
    prompts: Vec<String>,
    queued: Vec<String>,
    committed: Vec<String>,
    commands: Vec<String>,
    failure_at: Option<String>,
    invalidate_at: Option<String>,
    handled: bool,
    reported: usize,
}

impl TransferDependencies for QueueHost {
    type Error = String;
    fn take_batch(&mut self) -> Vec<CompactionQueuedMessage> { std::mem::take(&mut self.batch) }
    fn commit_accepted(&mut self, message: &CompactionQueuedMessage) -> bool {
        if self.invalidate_at.as_deref() == Some(&message.text) { return false; }
        self.committed.push(message.text.clone());
        true
    }
    fn restore_undelivered(&mut self, messages: &[CompactionQueuedMessage]) -> usize {
        self.restored = messages.iter().map(|message| message.text.clone()).collect();
        self.restored.len()
    }
    fn is_command(&self, message: &CompactionQueuedMessage) -> bool { message.text.starts_with('/') }
    async fn deliver_command(&mut self, message: &CompactionQueuedMessage) -> Result<(), String> {
        self.commands.push(message.text.clone());
        Ok(())
    }
    async fn deliver_first_prompt(&mut self, message: &CompactionQueuedMessage) -> Result<PromptDisposition, String> {
        if self.failure_at.as_deref() == Some(&message.text) { return Err("rejected".into()); }
        self.prompts.push(message.text.clone());
        Ok(if self.handled { PromptDisposition::Handled } else { PromptDisposition::Started })
    }
    async fn deliver_queued(&mut self, message: &CompactionQueuedMessage) -> Result<(), String> {
        if self.failure_at.as_deref() == Some(&message.text) { return Err("rejected".into()); }
        self.queued.push(message.text.clone());
        Ok(())
    }
    fn report_failure(&mut self, _: String, count: usize) { self.reported = count; }
}

fn messages(texts: &[&str]) -> Vec<CompactionQueuedMessage> {
    texts.iter().map(|text| CompactionQueuedMessage { text: (*text).into(), mode: StreamingBehavior::Steer, enqueue_order: None, pending_echo_id: None }).collect()
}

#[tokio::test]
async fn transfer_restores_only_unaccepted_suffix() {
    let mut host = QueueHost { batch: messages(&["first", "failed", "last"]), failure_at: Some("failed".into()), ..Default::default() };
    transfer_compaction_queue(&mut host, TransferOptions { will_retry: true, ..Default::default() }).await;
    assert_eq!(host.committed, ["first"]);
    assert_eq!(host.restored, ["failed", "last"]);
    assert_eq!(host.reported, 2);
}

#[tokio::test]
async fn deferred_admission_uses_native_queue_only() {
    let mut host = QueueHost { batch: messages(&["first", "last"]), ..Default::default() };
    transfer_compaction_queue(&mut host, TransferOptions { defer_admission: true, ..Default::default() }).await;
    assert!(host.prompts.is_empty());
    assert_eq!(host.queued, ["first", "last"]);
}

#[tokio::test]
async fn normal_transfer_admits_one_prompt_then_queues_remainder() {
    let mut host = QueueHost { batch: messages(&["first", "last"]), ..Default::default() };
    transfer_compaction_queue(&mut host, TransferOptions::default()).await;
    assert_eq!(host.prompts, ["first"]);
    assert_eq!(host.queued, ["last"]);
}

#[tokio::test]
async fn preflight_rejection_restores_whole_batch() {
    let mut host = QueueHost { batch: messages(&["first", "last"]), failure_at: Some("first".into()), ..Default::default() };
    transfer_compaction_queue(&mut host, TransferOptions::default()).await;
    assert_eq!(host.restored, ["first", "last"]);
    assert!(host.committed.is_empty());
}

#[tokio::test]
async fn handled_prompt_does_not_claim_prompt_work() {
    let mut host = QueueHost { batch: messages(&["first", "last"]), handled: true, ..Default::default() };
    transfer_compaction_queue(&mut host, TransferOptions::default()).await;
    assert_eq!(host.prompts, ["first", "last"]);
    assert!(host.queued.is_empty());
}

#[tokio::test]
async fn commands_never_claim_prompt_work() {
    let mut host = QueueHost { batch: messages(&["/command", "prompt"]), ..Default::default() };
    transfer_compaction_queue(&mut host, TransferOptions::default()).await;
    assert_eq!(host.commands, ["/command"]);
    assert_eq!(host.prompts, ["prompt"]);
}

#[tokio::test]
async fn session_invalidation_stops_delivery_without_restoring_stale_messages() {
    let mut host = QueueHost { batch: messages(&["first", "last"]), invalidate_at: Some("first".into()), ..Default::default() };
    transfer_compaction_queue(&mut host, TransferOptions::default()).await;
    assert_eq!(host.prompts, ["first"]);
    assert!(host.queued.is_empty());
    assert!(host.restored.is_empty());
}

#[test]
fn image_marker_removal_preserves_other_bracket_text() {
    use maho_interactive::editor_paste_transfer::strip_image_markers;
    assert_eq!(strip_image_markers("before [Image #1]   after [Image #29]"), "before after");
    assert_eq!(strip_image_markers("[Image #0] [Image #01] [Image #x]"), "[Image #0] [Image #01] [Image #x]");
}

struct PlainEditor { text: String }
impl maho_tui::tui::Component for PlainEditor {
    fn render(&mut self, _: usize) -> Vec<String> { vec![self.text.clone()] }
    fn invalidate(&mut self) {}
}
impl maho_tui::editor_component::EditorComponent for PlainEditor {
    fn get_text(&self) -> String { self.text.clone() }
    fn set_text(&mut self, text: &str) { self.text = text.into(); }
    fn get_expanded_text(&self) -> Option<String> { Some(self.text.clone()) }
}

#[test]
fn cleared_editor_submission_uses_authoritative_callback_value() {
    let editor = PlainEditor { text: String::new() };
    assert_eq!(maho_interactive::editor_paste_transfer::expand_submitted_text(&editor, "submitted"), "submitted");
}

#[test]
fn live_editor_submission_retains_custom_editor_content() {
    let editor = PlainEditor { text: "live".into() };
    assert_eq!(maho_interactive::editor_paste_transfer::expand_submitted_text(&editor, "submitted"), "live");
}

#[test]
fn transfer_to_plain_editor_strips_unowned_image_markers() {
    let source = PlainEditor { text: "draft [Image #1] text".into() };
    let mut target = PlainEditor { text: String::new() };
    assert!(!maho_interactive::editor_paste_transfer::transfer_editor_content(&source, &mut target));
    assert_eq!(target.text, "draft text");
}
