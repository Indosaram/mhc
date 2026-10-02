use maho_ai::types::{ContentBlock, Message, ToolCall, UserContent};

const READ: &[&str] = &["read", "Read", "read_file"];
const SEARCH: &[&str] = &["grep", "Grep", "glob", "Glob"];
const SHELL: &[&str] = &["bash", "Bash", "shell", "shell_execute"];
const CLEARABLE: &[&str] = &["read", "Read", "read_file", "write", "Write", "edit", "Edit", "grep", "Grep", "glob", "Glob", "bash", "Bash", "shell"];

#[derive(Clone, Debug)]
pub struct CollapseConsecutiveOptions {
    pub min_group_size: usize,
    pub protect_recent_messages: usize,
    pub read_tool_names: Vec<String>,
    pub search_tool_names: Vec<String>,
    pub shell_tool_names: Vec<String>,
}
impl Default for CollapseConsecutiveOptions {
    fn default() -> Self { Self { min_group_size: 2, protect_recent_messages: 5, read_tool_names: READ.iter().map(|s| (*s).into()).collect(), search_tool_names: SEARCH.iter().map(|s| (*s).into()).collect(), shell_tool_names: SHELL.iter().map(|s| (*s).into()).collect() } }
}
#[derive(Clone, Debug)]
pub struct MicroCompactAssistantOptions {
    pub protect_recent_tokens: usize,
    pub max_assistant_text_tokens: usize,
    pub min_savings_tokens: usize,
    pub replacement_template: String,
}
impl Default for MicroCompactAssistantOptions {
    fn default() -> Self { Self { protect_recent_tokens: 2000, max_assistant_text_tokens: 500, min_savings_tokens: 100, replacement_template: "[response shrunk — {original_tokens} → {shrunk_tokens} tokens]".into() } }
}
#[derive(Clone, Debug)]
pub struct ClearOldToolResultsOptions { pub keep_recent: usize, pub clearable_tool_names: Vec<String>, pub replacement_text: String }
impl Default for ClearOldToolResultsOptions {
    fn default() -> Self { Self { keep_recent: 3, clearable_tool_names: CLEARABLE.iter().map(|s| (*s).into()).collect(), replacement_text: "[tool result cleared]".into() } }
}
#[derive(Clone, Debug)]
pub struct ReduceContextOptions {
    pub collapse: Option<CollapseConsecutiveOptions>,
    pub shrink_assistant: Option<MicroCompactAssistantOptions>,
    pub clear_tool_results: Option<ClearOldToolResultsOptions>,
}
impl Default for ReduceContextOptions { fn default() -> Self { Self::all() } }
impl ReduceContextOptions {
    pub fn all() -> Self { Self { collapse: Some(Default::default()), shrink_assistant: Some(Default::default()), clear_tool_results: Some(Default::default()) } }
    pub fn builtin() -> Self {
        Self { collapse: Some(Default::default()), shrink_assistant: Some(MicroCompactAssistantOptions { protect_recent_tokens: 3000, max_assistant_text_tokens: 800, ..Default::default() }), clear_tool_results: Some(ClearOldToolResultsOptions { keep_recent: 6, ..Default::default() }) }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollapsedGroupKind { Read, Search, Shell }
#[derive(Clone, Debug)]
pub struct CollapsedGroup { pub kind: CollapsedGroupKind, pub count: usize, pub label: String, pub original_tokens: usize, pub collapsed_tokens: usize }
#[derive(Clone, Debug)]
pub struct ReductionResult {
    pub messages: Vec<Message>,
    pub tokens_saved: usize,
    pub groups: Vec<CollapsedGroup>,
    pub messages_modified: usize,
    pub tool_results_cleared: usize,
}
impl ReductionResult {
    fn new(messages: &[Message]) -> Self { Self { messages: messages.to_vec(), tokens_saved: 0, groups: Vec::new(), messages_modified: 0, tool_results_cleared: 0 } }
}
pub fn should_apply_context_reduction(usage: Option<f64>, window: f64, gate: Option<f64>, native: bool) -> bool {
    !native && window > 0.0 && usage.is_some_and(|usage| usage >= window * gate.unwrap_or(0.5))
}
fn tokens(text: &str) -> usize { text.encode_utf16().count().div_ceil(4) }
fn text(blocks: &[ContentBlock]) -> String {
    blocks.iter().filter_map(|block| match block { ContentBlock::Text(t) => Some(t.text.as_str()), _ => None }).collect()
}
fn message_text(message: &Message) -> String {
    match message {
        Message::User(user) => match &user.content { UserContent::Text(t) => t.clone(), UserContent::Blocks(b) => text(b) },
        Message::ToolResult(result) => text(&result.content),
        Message::Assistant(assistant) => assistant.content.iter().map(|block| match block {
            ContentBlock::Text(t) => t.text.clone(),
            ContentBlock::ToolCall(call) => format!("{} {}", call.name, serde_json::Value::Object(call.arguments.clone())),
            _ => String::new(),
        }).collect(),
        Message::ConfigurationUpdate(_) => String::new(),
    }
}
fn replace_result(message: &mut Message, replacement: &str) {
    if let Message::ToolResult(result) = message {
        let images = result.content.iter().filter(|block| matches!(block, ContentBlock::Image(_))).cloned();
        result.content = std::iter::once(ContentBlock::text(replacement)).chain(images).collect();
    }
}
fn classify(call: &ToolCall, options: &CollapseConsecutiveOptions) -> Option<CollapsedGroupKind> {
    if options.read_tool_names.contains(&call.name) { Some(CollapsedGroupKind::Read) }
    else if options.search_tool_names.contains(&call.name) { Some(CollapsedGroupKind::Search) }
    else if options.shell_tool_names.contains(&call.name) { Some(CollapsedGroupKind::Shell) } else { None }
}
fn hint(call: &ToolCall, kind: CollapsedGroupKind) -> Option<String> {
    let arg = |keys: &[&str]| keys.iter().find_map(|key| call.arguments.get(*key).and_then(serde_json::Value::as_str));
    let coalesce = |keys: &[&str]| keys.iter().find_map(|key| call.arguments.get(*key).filter(|value|!value.is_null())).and_then(serde_json::Value::as_str);
    let value = match kind {
        CollapsedGroupKind::Read => coalesce(&["path", "file_path", "filePath"]).map(str::to_owned),
        CollapsedGroupKind::Shell => coalesce(&["command", "cmd"]).map(str::to_owned),
        CollapsedGroupKind::Search => {
            let path = arg(&["path"]);
            let pattern = ["pattern", "glob", "query"].iter().find_map(|key| arg(&[*key]).filter(|s| !s.is_empty()));
            match (path.filter(|path|!path.is_empty()), pattern) { (Some(p), Some(q)) => Some(format!("{p}:{q}")), (Some(p), None) => Some(p.into()), (None, Some(q)) => Some(q.into()), (None, None) => None }
        }
    }?;
    if value.is_empty() { return None; }
    if value.encode_utf16().count() <= 80 { Some(value) } else { Some(format!("{}…", utf16_prefix(&value, 79))) }
}
fn utf16_prefix(text: &str, length: usize) -> String { String::from_utf16_lossy(&text.encode_utf16().take(length).collect::<Vec<_>>()) }
pub fn collapse_consecutive_tool_results(messages: &[Message], options: &CollapseConsecutiveOptions) -> ReductionResult {
    let mut result = ReductionResult::new(messages);
    let limit = messages.len().saturating_sub(options.protect_recent_messages);
    let mut operations = Vec::new();
    let mut i = 0;
    while i + 1 < limit {
        if let Message::Assistant(assistant) = &messages[i]
            && let Some(call) = assistant.content.iter().find_map(|block| match block { ContentBlock::ToolCall(c) => Some(c), _ => None })
            && let Message::ToolResult(next) = &messages[i + 1]
            && next.tool_call_id == call.id
            && let Some(kind) = classify(call, options) {
            operations.push((i, kind, hint(call, kind), text(&next.content))); i += 2;
        } else { i += 1; }
    }
    let mut start = 0;
    while start < operations.len() {
        let mut end = start + 1;
        while end < operations.len() && operations[end].1 == operations[end - 1].1 && operations[end].0 == operations[end - 1].0 + 2 { end += 1; }
        let group = &operations[start..end];
        if group.len() >= options.min_group_size.max(1) {
            let hints: Vec<_> = group.iter().filter_map(|op| op.2.as_deref()).take(5).collect();
            let noun = match group[0].1 { CollapsedGroupKind::Read => "read results", CollapsedGroupKind::Search => "search results", CollapsedGroupKind::Shell => "shell results" };
            let label = if hints.is_empty() { format!("[{} {noun}]", group.len()) } else {
                let more = group.len() - hints.len();
                format!("[{} {noun}: {}{}]", group.len(), hints.join(", "), if more > 0 { format!(", and {more} more") } else { String::new() })
            };
            let original_tokens = group.iter().map(|op| tokens(&op.3)).sum::<usize>();
            let collapsed_tokens = tokens(&label) * group.len();
            for op in group { replace_result(&mut result.messages[op.0 + 1], &label); }
            result.tokens_saved += original_tokens.saturating_sub(collapsed_tokens);
            result.groups.push(CollapsedGroup { kind: group[0].1, count: group.len(), label, original_tokens, collapsed_tokens });
        }
        start = end;
    }
    result
}
pub fn micro_compact_assistant_text(messages: &[Message], options: &MicroCompactAssistantOptions) -> ReductionResult {
    let mut result = ReductionResult::new(messages);
    let mut protected = 0;
    let mut recent = 0;
    for (i, message) in messages.iter().enumerate().rev() {
        let count = tokens(&message_text(message));
        if recent + count > options.protect_recent_tokens { protected = i + 1; break; }
        recent += count;
    }
    for message in &mut result.messages[..protected] {
        let Message::Assistant(assistant) = message else { continue; };
        if assistant.content.is_empty() || !assistant.content.iter().all(|b| matches!(b, ContentBlock::Text(_))) { continue; }
        let original = assistant.content.iter().filter_map(|b| match b { ContentBlock::Text(t) => Some(t.text.as_str()), _ => None }).collect::<Vec<_>>().join("\n");
        let original_tokens = tokens(&original);
        if original_tokens <= options.max_assistant_text_tokens { continue; }
        let target = ((options.max_assistant_text_tokens as f64 * 0.3).floor() as usize).max(1);
        let chars = (original.encode_utf16().count() as f64 * target as f64 / original_tokens as f64).floor() as usize;
        let prefix = utf16_prefix(&original, chars);
        let mut shrunk_tokens = 0;
        let mut candidate = String::new();
        for _ in 0..5 {
            let marker = options.replacement_template.replace("{original_tokens}", &original_tokens.to_string()).replace("{shrunk_tokens}", &shrunk_tokens.to_string());
            candidate = if prefix.is_empty() { marker } else { format!("{prefix}\n\n{marker}") };
            let count = tokens(&candidate);
            if count == shrunk_tokens { break; }
            shrunk_tokens = count;
        }
        let saved = original_tokens.saturating_sub(tokens(&candidate));
        if saved < options.min_savings_tokens { continue; }
        assistant.content = vec![ContentBlock::text(candidate)];
        result.tokens_saved += saved; result.messages_modified += 1;
    }
    result
}
pub fn clear_old_tool_results(messages: &[Message], options: &ClearOldToolResultsOptions) -> ReductionResult {
    let mut result = ReductionResult::new(messages);
    let indices: Vec<_> = messages.iter().enumerate().filter_map(|(i, message)| match message { Message::ToolResult(r) if options.clearable_tool_names.contains(&r.tool_name) => Some(i), _ => None }).collect();
    for &i in indices.iter().take(indices.len().saturating_sub(options.keep_recent)) {
        if let Message::ToolResult(original) = &messages[i] { result.tokens_saved += tokens(&text(&original.content)).saturating_sub(tokens(&options.replacement_text)); }
        replace_result(&mut result.messages[i], &options.replacement_text); result.tool_results_cleared += 1;
    }
    result
}
pub fn reduce_context_messages(messages: &[Message], options: &ReduceContextOptions) -> ReductionResult {
    let mut result = ReductionResult::new(messages);
    if let Some(options) = &options.collapse { result = collapse_consecutive_tool_results(&result.messages, options); }
    if let Some(options) = &options.shrink_assistant {
        let shrunk = micro_compact_assistant_text(&result.messages, options);
        result.messages = shrunk.messages; result.tokens_saved += shrunk.tokens_saved; result.messages_modified += shrunk.messages_modified;
    }
    if let Some(options) = &options.clear_tool_results {
        let cleared = clear_old_tool_results(&result.messages, options);
        result.messages = cleared.messages; result.tokens_saved += cleared.tokens_saved; result.tool_results_cleared += cleared.tool_results_cleared;
    }
    result
}
