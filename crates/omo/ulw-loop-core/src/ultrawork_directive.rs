//! Standalone ultrawork directive injection.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ultrawork_skill_pointer::{build_ultrawork_additional_context, UltraworkAdditionalContextOptions};

#[derive(Clone, Debug)]
pub struct UltraworkDirectiveInput<'a> {
    pub prompt: &'a str,
    pub transcript_path: Option<&'a Path>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserPromptSubmitHookOutput {
    #[serde(rename = "hookSpecificOutput")]
    pub hook_specific_output: HookSpecificOutput,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HookSpecificOutput {
    #[serde(rename = "hookEventName")]
    pub hook_event_name: String,
    #[serde(rename = "additionalContext")]
    pub additional_context: String,
}

const ULTRAWORK_DIRECTIVE_MARKER: &str = "<ultrawork-mode>";
const TRANSCRIPT_SEARCH_BYTES: u64 = 512_000;
const CONTEXT_PRESSURE_MARKERS: [&str; 7] = [
    "context compacted",
    "context_length_exceeded",
    "skill descriptions were shortened",
    "context_too_large",
    "codex ran out of room in the model's context window",
    "your input exceeds the context window",
    "long threads and multiple compactions",
];

pub fn build_ultrawork_directive_output(
    input: &UltraworkDirectiveInput,
    options: &UltraworkAdditionalContextOptions,
) -> String {
    if is_context_pressure_recovery_prompt(input.prompt) {
        return String::new();
    }
    if has_ultrawork_directive_already_in_transcript(input.transcript_path) {
        return String::new();
    }
    if is_context_pressure_transcript(input.transcript_path) {
        return String::new();
    }
    if is_ultrawork_prompt(input.prompt) {
        format_additional_context_output(&build_ultrawork_additional_context(options))
    } else {
        String::new()
    }
}

fn has_ultrawork_directive_already_in_transcript(transcript_path: Option<&Path>) -> bool {
    let Some(path) = transcript_path else {
        return false;
    };
    let Ok(tail) = read_transcript_tail(path) else {
        return false;
    };

    for line in tail.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(parsed) = serde_json::from_str::<Value>(line) {
            if let Some(hso) = parsed.get("hookSpecificOutput") {
                if hso.get("hookEventName").and_then(|v| v.as_str()) == Some("UserPromptSubmit") {
                    if let Some(ac) = hso.get("additionalContext").and_then(|v| v.as_str()) {
                        if ac.contains(ULTRAWORK_DIRECTIVE_MARKER) {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

fn read_transcript_tail(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let seek_pos = len.saturating_sub(TRANSCRIPT_SEARCH_BYTES);
    file.seek(SeekFrom::Start(seek_pos))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).to_string())
}

fn is_ultrawork_prompt(prompt: &str) -> bool {
    let re = Regex::new(r"(?i)(?:ultrawork|ulw)").expect("valid regex");
    re.is_match(prompt)
}

fn is_context_pressure_recovery_prompt(prompt: &str) -> bool {
    let lower = prompt.to_lowercase();
    CONTEXT_PRESSURE_MARKERS.iter().any(|m| lower.contains(m))
}

fn is_context_pressure_transcript(transcript_path: Option<&Path>) -> bool {
    let Some(path) = transcript_path else {
        return false;
    };
    let Ok(tail) = read_transcript_tail(path) else {
        return false;
    };
    is_context_pressure_recovery_prompt(&tail)
}

fn format_additional_context_output(additional_context: &str) -> String {
    let normalized = normalize_additional_context(additional_context);
    if normalized.is_empty() {
        return String::new();
    }
    let output = UserPromptSubmitHookOutput {
        hook_specific_output: HookSpecificOutput {
            hook_event_name: "UserPromptSubmit".to_string(),
            additional_context: normalized,
        },
    };
    let json = serde_json::to_string(&output).unwrap_or_default();
    format!("{json}\n")
}

fn normalize_additional_context(additional_context: &str) -> String {
    additional_context.replace("\r\n", "\n").replace('\r', "\n").trim().to_string()
}
