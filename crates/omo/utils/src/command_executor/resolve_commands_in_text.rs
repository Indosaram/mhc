use std::thread;

use super::embedded_commands::find_embedded_commands;
use super::execute_command::execute_command;

const DEFAULT_MAX_DEPTH: u32 = 3;

pub fn resolve_commands_in_text(text: &str) -> String {
    resolve_commands_in_text_with_depth(text, 0, DEFAULT_MAX_DEPTH)
}

/// Replace every embedded command with its output (commands run in parallel), re-resolving up to `max_depth`.
pub fn resolve_commands_in_text_with_depth(text: &str, depth: u32, max_depth: u32) -> String {
    if depth >= max_depth {
        return text.to_string();
    }
    let matches = find_embedded_commands(text);
    if matches.is_empty() {
        return text.to_string();
    }
    let outputs: Vec<String> = thread::scope(|scope| {
        let handles: Vec<_> = matches
            .iter()
            .map(|found| scope.spawn(|| execute_command(&found.command)))
            .collect();
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|_| "[error: command thread panicked]".to_string())
            })
            .collect()
    });
    let mut resolved = text.to_string();
    let mut replaced = std::collections::HashSet::new();
    for (found, output) in matches.iter().zip(outputs) {
        if replaced.insert(found.full_match.clone()) {
            resolved = resolved.replace(&found.full_match, &output);
        }
    }
    if find_embedded_commands(&resolved).is_empty() {
        resolved
    } else {
        resolve_commands_in_text_with_depth(&resolved, depth + 1, max_depth)
    }
}
