use std::collections::BTreeMap;
use serde_json::Value;
use crate::theme::{Theme, ThemeColor};
use maho_ai::utils::{provider_failure_description::{describe_provider_failure_for_user, strip_turn_retry_suppression_prefix}, retry::ProviderStallDescriptionOptions};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DescriptorKind { Spacer, TextMarkdown, ThinkingMarkdown, ThinkingLabel, ProviderNativeSummary, ProviderNativeBody, ErrorText }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssistantRenderDescriptor { pub kind: DescriptorKind, pub text: String, pub thinking_run: Option<usize> }
pub struct AssistantRenderDescriptorOptions<'a> {
    pub expanded: bool,
    pub provider_error_owned: bool,
    pub hidden_thinking_label: &'a str,
    pub hide_thinking_block: bool,
    pub thinking_visibility_overrides: &'a BTreeMap<usize, bool>,
    pub has_tool_calls: bool,
}
fn visible(content: &Value, provider_native: bool) -> bool {
    match content["type"].as_str() {
        Some("text") => content["text"].as_str().is_some_and(|text| !text.trim().is_empty()),
        Some("thinking") => content["thinking"].as_str().is_some_and(|text| !text.trim().is_empty()),
        Some("providerNative") => provider_native,
        _ => false,
    }
}
fn duration(ms: f64) -> String {
    if ms < 1000. { format!("{ms}ms") }
    else if ms < 60000. { format!("{:.1}s", ms / 1000.) }
    else if ms < 3600000. { format!("{}m {}s", (ms / 60000.).floor(), ((ms % 60000.) / 1000.).floor()) }
    else if ms < 86400000. { format!("{}h {}m", (ms / 3600000.).floor(), ((ms % 3600000.) / 60000.).floor()) }
    else { format!("{}d {}h", (ms / 86400000.).floor(), ((ms % 86400000.) / 3600000.).floor()) }
}
fn descriptor(kind: DescriptorKind, text: String, thinking_run: Option<usize>) -> AssistantRenderDescriptor {
    AssistantRenderDescriptor { kind, text, thinking_run }
}
fn spacer() -> AssistantRenderDescriptor { descriptor(DescriptorKind::Spacer, String::new(), None) }

pub fn create_assistant_render_descriptors(message: &Value, options: &AssistantRenderDescriptorOptions<'_>, theme: &Theme) -> Vec<AssistantRenderDescriptor> {
    let content = message["content"].as_array().map_or(&[][..], Vec::as_slice);
    let mut descriptors = Vec::new();
    if content.iter().any(|c| visible(c, true)) { descriptors.push(spacer()); }
    let mut index = 0;
    let mut thinking_run = 0;
    while let Some(part) = content.get(index) {
        match part["type"].as_str() {
            Some("text") => {
                let text = part["text"].as_str().unwrap_or_default().trim();
                if !text.is_empty() { descriptors.push(descriptor(DescriptorKind::TextMarkdown, text.into(), None)); }
            }
            Some("thinking") => {
                let mut blocks = Vec::new();
                let mut timing = false;
                let mut done = true;
                let mut start = f64::INFINITY;
                let mut end = f64::NEG_INFINITY;
                while let Some(part) = content.get(index).filter(|part| part["type"] == "thinking") {
                    if let Some(started) = part["startedAt"].as_f64() {
                        timing = true;
                        start = start.min(started);
                        if let Some(ended) = part["endedAt"].as_f64() { end = end.max(ended); } else { done = false; }
                    }
                    let text = part["thinking"].as_str().unwrap_or_default().trim();
                    if !text.is_empty() { blocks.push(text); }
                    index += 1;
                }
                if blocks.is_empty() { continue; }
                let run = thinking_run;
                thinking_run += 1;
                let hidden = options.thinking_visibility_overrides.get(&run).copied().unwrap_or(options.hide_thinking_block);
                let label = if timing && done { format!("Thought: {}", duration((end - start).max(0.))) } else { options.hidden_thinking_label.to_owned() };
                let label = theme.italic(&theme.fg(ThemeColor::ThinkingText, &label));
                if hidden || timing { descriptors.push(descriptor(DescriptorKind::ThinkingLabel, label, Some(run))); }
                if !hidden { descriptors.push(descriptor(DescriptorKind::ThinkingMarkdown, blocks.join("\n\n"), Some(run))); }
                if content[index..].iter().any(|c| visible(c, false)) { descriptors.push(spacer()); }
                continue;
            }
            Some("providerNative") => {
                let marker = if options.expanded { "▾" } else { "▸" };
                let provider = message["provider"].as_str().filter(|p| !p.is_empty()).map_or_else(String::new, |p| format!("{p} · "));
                let summary = format!("{marker} {provider}providerNative · {}", part["subtype"].as_str().unwrap_or_default());
                let body = serde_json::to_string_pretty(&part["raw"]).unwrap_or_else(|_| "null".into());
                let body = if !options.expanded && body.encode_utf16().count() > 2000 { format!("{}…", body.chars().take(2000).collect::<String>()) } else { body };
                descriptors.push(descriptor(DescriptorKind::ProviderNativeSummary, theme.fg(ThemeColor::Muted, &summary), None));
                descriptors.push(descriptor(DescriptorKind::ProviderNativeBody, theme.fg(ThemeColor::Dim, &body), None));
                if content[index + 1..].iter().any(|c| visible(c, true)) { descriptors.push(spacer()); }
            }
            _ => {}
        }
        index += 1;
    }
    let error_message = message["errorMessage"].as_str();
    let error = match message["stopReason"].as_str() {
        Some("length") => Some("Error: Model stopped because it reached the maximum output token limit. The response may be incomplete.".into()),
        Some("aborted") if !options.has_tool_calls && !options.provider_error_owned => Some(error_message.filter(|message| !message.is_empty() && *message != "Request was aborted").map_or_else(|| "Operation aborted".into(), strip_turn_retry_suppression_prefix)),
        Some("error") if !options.has_tool_calls && !options.provider_error_owned && !message["diagnostics"].as_array().is_some_and(|ds| ds.iter().any(|d| d["type"] == "server_fallback_aborted")) => {
            Some(describe_provider_failure_for_user(error_message, &ProviderStallDescriptionOptions::default()).unwrap_or_else(|| {
                let stripped = strip_turn_retry_suppression_prefix(error_message.unwrap_or_default());
                format!("Error: {}", if stripped.is_empty() { "Unknown error" } else { &stripped })
            }))
        }
        _ => None,
    };
    if let Some(error) = error { descriptors.push(spacer()); descriptors.push(descriptor(DescriptorKind::ErrorText, theme.fg(ThemeColor::Error, &error), None)); }
    descriptors
}
