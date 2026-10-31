use super::*;

#[test]
fn image_lines_are_detected_at_the_start_and_inline() {
    assert!(is_image_line("\x1b_Ga=T;data\x1b\\"));
    assert!(is_image_line("\x1b]1337;File=inline=1:data\x07"));
    assert!(is_image_line("\x1b[3A\x1b_Ga=T;data\x1b\\"));
    assert!(!is_image_line("plain text"));
}

#[test]
fn image_rows_use_the_default_cell_size_when_none_is_given() {
    let image = ImageDimensions {
        width_px: 90,
        height_px: 180,
    };
    assert_eq!(calculate_image_rows(image, 10.0, None), 10);
    assert_eq!(
        calculate_image_rows(
            image,
            10.0,
            Some(CellDimensions {
                width_px: 9,
                height_px: 9
            })
        ),
        20
    );
}

// VS Code terminal without TERM_PROGRAM (#2826)
const REMOTE_WSL_VS_CODE: [(&str, &str); 3] = [
    ("TERM", "xterm-256color"),
    ("VSCODE_IPC_HOOK_CLI", "/run/user/1000/vscode-ipc-1.sock"),
    ("VSCODE_GIT_IPC_HANDLE", "/run/user/1000/vscode-git-1.sock"),
];

fn no_tmux_hyperlinks() -> bool {
    false
}

fn vs_code_env(extra: &[(&'static str, &'static str)]) -> Env {
    let mut pairs: Vec<(&str, &str)> = REMOTE_WSL_VS_CODE.to_vec();
    for (key, value) in extra {
        pairs.retain(|(k, _)| k != key);
        pairs.push((key, value));
    }
    crate::process_env::env_from(&pairs)
}

fn detect(env: &Env) -> DetectedTerminalCapabilities {
    detect_capabilities_in(env, "linux", no_tmux_hyperlinks)
}

#[test]
fn vs_code_environment_without_term_program_turns_hyperlinks_on() {
    assert!(detect(&vs_code_env(&[])).hyperlinks);
}

#[test]
fn the_git_extension_handle_alone_is_not_the_vs_code_terminal() {
    let env = crate::process_env::env_from(&[
        ("TERM", "xterm-256color"),
        ("VSCODE_GIT_IPC_HANDLE", "/run/user/1000/vscode-git-1.sock"),
    ]);
    assert!(!detect(&env).hyperlinks);
}

#[test]
fn plain_xterm_without_vs_code_keeps_hyperlinks_off() {
    let env = crate::process_env::env_from(&[("TERM", "xterm-256color")]);
    assert!(!detect(&env).hyperlinks);
}

#[test]
fn pi_hyperlinks_zero_switches_them_off_even_in_vs_code() {
    assert!(!detect(&vs_code_env(&[("PI_HYPERLINKS", "0")])).hyperlinks);
}

#[test]
fn another_terminal_with_term_program_wins_over_the_vs_code_marker() {
    assert!(!detect(&vs_code_env(&[("TERM_PROGRAM", "someterm")])).hyperlinks);
}

#[test]
fn screen_inside_vs_code_keeps_hyperlinks_off() {
    assert!(!detect(&vs_code_env(&[("TERM", "screen-256color")])).hyperlinks);
    assert!(
        !detect(&vs_code_env(&[
            ("TERM", "screen-256color"),
            ("TERM_PROGRAM", "vscode")
        ]))
        .hyperlinks
    );
}

#[test]
fn jetbrains_wins_over_the_inherited_vs_code_marker() {
    assert!(!detect(&vs_code_env(&[("TERMINAL_EMULATOR", "JetBrains-JediTerm")])).hyperlinks);
}

#[test]
fn herdr_pane_started_from_vs_code_keeps_the_probe() {
    let env = crate::process_env::env_from(&[
        ("HERDR_ENV", "1"),
        ("TERM", "xterm-256color"),
        ("VSCODE_IPC_HOOK_CLI", "/run/user/1000/vscode-ipc-1.sock"),
    ]);
    let caps = detect(&env);
    assert!(!caps.hyperlinks);
    if caps.images.is_none() {
        assert_eq!(caps.image_probe, Some(ImageProbe::Kitty));
    }
}
