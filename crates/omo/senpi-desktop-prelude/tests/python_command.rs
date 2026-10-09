fn python_command_for_platform(platform: &str) -> &'static str {
    if platform == "win32" || platform == "windows" {
        "python"
    } else {
        "python3"
    }
}

#[test]
fn uses_the_setup_python_command_on_windows() {
    assert_eq!(python_command_for_platform("win32"), "python");
    assert_eq!(python_command_for_platform("windows"), "python");
}

#[test]
fn keeps_the_python3_command_on_posix_platforms() {
    assert_eq!(python_command_for_platform("darwin"), "python3");
    assert_eq!(python_command_for_platform("linux"), "python3");
}
