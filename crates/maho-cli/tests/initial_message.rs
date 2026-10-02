use maho_cli::cli::{args::Args, initial_message::build_initial_message};
fn args(messages: &[&str]) -> Args { Args { messages: messages.iter().map(|message| (*message).to_owned()).collect(), ..Default::default() } }
#[test]
fn merges_stdin_and_first_message() {
    let mut parsed = args(&["Summarize the text given"]);
    let result = build_initial_message(&mut parsed, None, None, Some("README contents\n"));
    assert_eq!(result.initial_message.as_deref(), Some("README contents\nSummarize the text given"));
    assert!(result.initial_title_prompt.is_none()); assert!(parsed.messages.is_empty());
}
#[test]
fn uses_stdin_without_cli_message() {
    let mut parsed = args(&[]);
    let result = build_initial_message(&mut parsed, None, None, Some("README contents"));
    assert_eq!(result.initial_message.as_deref(), Some("README contents"));
    assert!(result.initial_title_prompt.is_none());
}
#[test]
fn combines_stdin_file_and_first_message() {
    let mut parsed = args(&["Explain it", "Second message"]);
    let result = build_initial_message(&mut parsed, Some("file\n"), None, Some("stdin\n"));
    assert_eq!(result.initial_message.as_deref(), Some("stdin\nfile\nExplain it"));
    assert!(result.initial_title_prompt.is_none()); assert_eq!(parsed.messages, ["Second message"]);
}
#[test]
fn titles_from_public_first_message() {
    let mut parsed = args(&["Explain the architecture"]);
    let result = build_initial_message(&mut parsed, None, None, None);
    assert_eq!(result.initial_title_prompt.as_deref(), Some("Explain the architecture"));
    assert_eq!(result.initial_message, result.initial_title_prompt); assert!(parsed.messages.is_empty());
}
#[test]
fn excludes_private_file_text_from_title() {
    let mut parsed = args(&["Summarize this"]);
    let result = build_initial_message(&mut parsed, Some("private file contents\n"), None, None);
    assert_eq!(result.initial_message.as_deref(), Some("private file contents\nSummarize this"));
    assert!(result.initial_title_prompt.is_none()); assert!(parsed.messages.is_empty());
}
