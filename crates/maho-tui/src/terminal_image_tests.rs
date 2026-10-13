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
    let image = ImageDimensions { width_px: 90, height_px: 180 };
    assert_eq!(calculate_image_rows(image, 10.0, None), 10);
    assert_eq!(calculate_image_rows(image, 10.0, Some(CellDimensions { width_px: 9, height_px: 9 })), 20);
}
