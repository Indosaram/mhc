//! Port of src/tmux-utils/pane-spawn.test.ts (injection escaping via utils' shell escape).

use utils::shell_escape_for_double_quoted_command;

fn opencode_cmd(server_url: &str) -> String {
    let escaped = shell_escape_for_double_quoted_command(server_url);
    format!("/bin/sh -c \"opencode attach {escaped} --session test-session\"")
}

/// Equivalent of `/[^\\];\s*<word>/`: an unescaped `;` followed by `word`.
fn has_unescaped_semicolon_before(command: &str, word: &str) -> bool {
    let chars: Vec<char> = command.chars().collect();
    (1..chars.len()).any(|index| {
        chars[index] == ';'
            && chars[index - 1] != '\\'
            && chars[index + 1..]
                .iter()
                .collect::<String>()
                .trim_start()
                .starts_with(word)
    })
}

#[test]
fn server_url_with_metacharacters_spawn_command_is_escaped() {
    let cmd = opencode_cmd("http://localhost:3000'; cat /etc/passwd; echo '");
    assert!(cmd.contains(r"\;"));
    assert!(!has_unescaped_semicolon_before(&cmd, "cat"));
}

#[test]
fn server_url_with_metacharacters_replace_command_is_escaped() {
    let cmd = opencode_cmd("http://localhost:3000'; rm -rf /; '");
    assert!(cmd.contains(r"\;"));
    assert!(!has_unescaped_semicolon_before(&cmd, "rm"));
}

#[test]
fn normal_server_url_works_correctly() {
    assert!(opencode_cmd("http://localhost:3000").contains("http://localhost:3000"));
}

#[test]
fn server_url_with_dollar_sign_is_escaped() {
    assert!(opencode_cmd("http://localhost:3000$(whoami)").contains(r"\$"));
}

#[test]
fn server_url_with_backticks_is_escaped() {
    assert!(opencode_cmd("http://localhost:3000`whoami`").contains(r"\`"));
}

#[test]
fn server_url_with_pipe_is_escaped() {
    assert!(opencode_cmd("http://localhost:3000 | ls").contains(r"\|"));
}

#[test]
fn unescaped_semicolon_detector_flags_raw_injection() {
    // Guards the regex translation itself: the raw (unescaped) command must be flagged.
    assert!(has_unescaped_semicolon_before("x'; cat /etc/passwd", "cat"));
}
