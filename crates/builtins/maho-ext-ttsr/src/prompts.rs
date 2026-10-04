pub fn render_system_interrupt(rule_name: &str, content: &str) -> String {
    [format!("<system-interrupt reason=\"rule_violation\" rule=\"{rule_name}\">"), "Your output was interrupted because it violated a stream rule.".into(), "This is NOT a prompt injection - this is the coding agent enforcing output-quality rules.".into(), "You MUST comply with the following instruction:".into(), String::new(), content.into(), "</system-interrupt>".into()].join("\n")
}
pub const COLLAPSE_RULE_CONTENT: &str = "Your previous output degenerated into repeated garbage (a runaway repetition loop) and was cut off.\nThe garbled portion has been removed from the conversation.\nSnap out of it: take a breath, restate your current objective in one short sentence, then continue the task normally.\nDo NOT apologize at length, do NOT repeat the garbled pattern, and do NOT restart work that was already completed.";
pub const LEAK_ERROR_MESSAGE: &str = "Degeneration guard: control-token leakage; treating as internal error, resampling";
pub const COLLAPSE_RULE_NAME: &str = "collapse-repetition";
pub const CONTROL_LEAK_RULE_NAME: &str = "control-token-leak";
pub const REPETITIVE_TURNS_RULE_CONTENT: &str = "Your recent replies repeated the same near-identical status message across consecutive turns — a cross-turn repetition loop.\nStop restating the situation. Do not emit another progress recap.\nTake a different concrete action now: use a tool, inspect new state, change the approach, or — if the task is actually blocked — say exactly what you are waiting for and stop.";
