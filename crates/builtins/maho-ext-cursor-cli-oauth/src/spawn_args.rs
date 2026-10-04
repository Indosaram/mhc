#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExecutionMode { #[default] Agent, Plan }

#[derive(Default)]
pub struct CursorCliArgsInput<'a> {
    pub prompt: &'a str,
    pub model: Option<&'a str>,
    pub resume_chat_id: Option<&'a str>,
    pub force: bool,
    pub execution_mode: ExecutionMode,
    pub sandbox_mode: Option<&'a str>,
}

/// Serialize print-mode arguments in source order without applying execution policy.
pub fn build_cursor_cli_args(input: CursorCliArgsInput<'_>) -> Vec<String> {
    let mut args: Vec<String> = ["-p", input.prompt, "--output-format", "stream-json", "--stream-partial-output", "--trust"]
        .into_iter().map(String::from).collect();
    for (flag, value) in [("--model", input.model), ("--resume", input.resume_chat_id)] {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            args.extend([flag.into(), value.into()]);
        }
    }
    if input.force { args.push("--force".into()); }
    if input.execution_mode == ExecutionMode::Plan { args.extend(["--mode".into(), "plan".into()]); }
    if let Some(mode) = input.sandbox_mode.filter(|value| !value.is_empty()) {
        args.extend(["--sandbox".into(), mode.into()]);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    fn expected(prompt: &str, suffix: &[&str]) -> Vec<String> {
        ["-p", prompt, "--output-format", "stream-json", "--stream-partial-output", "--trust"]
            .into_iter().chain(suffix.iter().copied()).map(String::from).collect()
    }
    #[test]
    fn fresh_invocation() {
        assert_eq!(build_cursor_cli_args(CursorCliArgsInput { prompt: "Implement", model: Some("cursor-large"), force: true, ..Default::default() }), expected("Implement", &["--model", "cursor-large", "--force"]));
    }
    #[test]
    fn resume_follows_model() {
        assert_eq!(build_cursor_cli_args(CursorCliArgsInput { prompt: "Continue", model: Some("cursor-small"), resume_chat_id: Some("chat-123"), force: true, ..Default::default() }), expected("Continue", &["--model", "cursor-small", "--resume", "chat-123", "--force"]));
    }
    #[test]
    fn plan_serialization_does_not_apply_policy() {
        assert_eq!(build_cursor_cli_args(CursorCliArgsInput { prompt: "Plan", force: true, execution_mode: ExecutionMode::Plan, ..Default::default() }), expected("Plan", &["--force", "--mode", "plan"]));
    }
    #[test]
    fn false_force_omitted() {
        assert_eq!(build_cursor_cli_args(CursorCliArgsInput { prompt: "Ask", ..Default::default() }), expected("Ask", &[]));
    }
    #[test]
    fn sandbox_last() {
        assert_eq!(build_cursor_cli_args(CursorCliArgsInput { prompt: "Run", model: Some("cursor-large"), resume_chat_id: Some("chat-456"), force: true, execution_mode: ExecutionMode::Plan, sandbox_mode: Some("workspace-write") }), expected("Run", &["--model", "cursor-large", "--resume", "chat-456", "--force", "--mode", "plan", "--sandbox", "workspace-write"]));
    }
    #[test]
    fn shell_text_stays_one_argument() {
        let prompt = "Review; echo $(whoami) --force --resume stolen-chat";
        assert_eq!(build_cursor_cli_args(CursorCliArgsInput { prompt, ..Default::default() }), expected(prompt, &[]));
    }
    #[test]
    fn adversarial_prompts_preserved() {
        for prompt in ["", "line one\nline two", "before\0after", "low\u{1}high\u{7f}"] {
            assert_eq!(build_cursor_cli_args(CursorCliArgsInput { prompt, ..Default::default() }), expected(prompt, &[]));
        }
    }
    #[test]
    fn never_adds_auth_or_yolo_flags() {
        let args = build_cursor_cli_args(CursorCliArgsInput { prompt: "Authenticate", model: Some("cursor-large"), resume_chat_id: Some("chat-789"), force: true, execution_mode: ExecutionMode::Plan, sandbox_mode: Some("workspace-write") });
        assert!(!args.iter().any(|arg| arg == "--api-key" || arg == "--yolo" || arg.to_lowercase().contains("env")));
    }
    #[test]
    fn empty_optional_values_omitted() {
        assert_eq!(build_cursor_cli_args(CursorCliArgsInput { prompt: "", model: Some(""), resume_chat_id: Some(""), sandbox_mode: Some(""), ..Default::default() }), expected("", &[]));
    }
}
