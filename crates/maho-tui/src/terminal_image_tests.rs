use super::*;

#[test]
fn image_lines_are_detected_at_the_start_and_inline() {
    assert!(is_image_line("\x1b_Ga=T;data\x1b\\"));
    assert!(is_image_line("\x1b]1337;File=inline=1:data\x07"));
    assert!(is_image_line("\x1b[3A\x1b_Ga=T;data\x1b\\"));
    assert!(!is_image_line("plain text"));
}
