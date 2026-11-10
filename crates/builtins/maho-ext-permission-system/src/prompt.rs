use crate::types::{Reply, ReplyInput, Request};
use maho_ext_api::{ExtensionContext, ExtensionUiDialogOptions};
use serde_json::Value;

fn display(value: Option<&Value>) -> String {
    value
        .filter(|value| !value.is_null() && **value != false && **value != "")
        .map_or_else(
            || "Unknown".into(),
            |value| {
                value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_owned)
            },
        )
}
pub fn format_request_for_display(request: &Request) -> String {
    let meta = &request.metadata;
    let mut parts = Vec::new();
    match request.permission.as_str() {
        "edit" => parts.push(format!("File: {}", display(meta.get("filepath")))),
        "read" => parts.push(format!("Path: {}", display(meta.get("filePath")))),
        "glob" | "grep" => parts.push(format!("Pattern: {}", display(meta.get("pattern")))),
        "list" => parts.push(format!("Path: {}", display(meta.get("path")))),
        "bash" => {
            if let Some(description) = meta
                .get("description")
                .filter(|value| !value.is_null() && **value != "")
            {
                parts.push(format!("Description: {}", display(Some(description))));
            }
            parts.push(format!("Command: $ {}", display(meta.get("command"))));
        }
        "websearch" | "codesearch" => parts.push(format!("Query: {}", display(meta.get("query")))),
        "external_directory" => {
            // These are resolved targets, not approval globs. Caller metadata cannot name the
            // approved directory, and a literal '*' in a filename must survive (senpi#4316).
            if request.patterns.is_empty() {
                parts.push("Directory: Unknown".into());
            }
            for target in &request.patterns {
                parts.push(format!("Directory: {target}"));
            }
            if let Some(original) = request
                .original_paths
                .as_ref()
                .filter(|paths| !paths.is_empty())
            {
                parts.push(format!(
                    "Requested paths:\n{}",
                    original
                        .iter()
                        .map(|path| format!("  - {path}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                ));
            }
        }
        _ => parts.push(format!("Tool: {}", request.permission)),
    }
    if !request.patterns.is_empty() {
        parts.push(format!(
            "\nPatterns:\n{}",
            request
                .patterns
                .iter()
                .map(|pattern| format!("  - {pattern}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    parts.join("\n")
}
pub async fn show_permission_prompt(ctx: &ExtensionContext, request: &Request) -> ReplyInput {
    let title = format!(
        "Permission required: {}\n\n{}",
        request.permission,
        format_request_for_display(request)
    );
    // senpi `showPermissionPrompt`: the wider scope is a separate labelled choice, never a silent
    // widening of "Allow always" (senpi#3403).
    let wider = request.wider_always.as_ref();
    let mut labels = vec!["Allow once".to_owned(), "Allow always".to_owned()];
    if let Some(wider) = wider {
        labels.push(wider.label.clone());
    }
    labels.push("Deny".into());
    labels.push("Deny with feedback".into());
    // Clients bind the dialog to the call it approves by this id; with several calls of one tool in
    // flight (the engine prepares a message's calls before running them), nothing else says which.
    let call = ExtensionUiDialogOptions {
        tool_call_id: request.tool.as_ref().map(|tool| tool.call_id.clone()),
        parent_tool_call_id: request
            .tool
            .as_ref()
            .and_then(|tool| tool.parent_call_id.clone()),
        ..Default::default()
    };
    let choice = ctx.ui.select(&title, &labels, call.clone()).await;
    let (reply, message, always) = match choice.as_deref() {
        Some("Allow once") => (Reply::Once, None, None),
        Some("Allow always") => (Reply::Always, None, None),
        Some("Deny with feedback") => (
            Reply::Reject,
            ctx.ui
                .input(
                    "Feedback",
                    Some("Why are you denying this permission? (optional)"),
                    call,
                )
                .await
                .filter(|message| !message.is_empty()),
            None,
        ),
        // A labelled wider scope stores its own patterns, not the request's exact-arg ones.
        Some(chosen) if wider.is_some_and(|scope| scope.label == chosen) => (
            Reply::Always,
            None,
            wider.map(|scope| scope.patterns.clone()),
        ),
        _ => (Reply::Reject, None, None),
    };
    ReplyInput {
        request_id: request.id.clone(),
        reply,
        message,
        always,
    }
}
