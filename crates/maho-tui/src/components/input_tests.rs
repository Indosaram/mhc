use super::*;

fn left_press(x: i64) -> TuiMouseEvent {
    TuiMouseEvent {
        event_type: TuiMouseEventType::Press,
        button: TuiMouseButton::Left,
        x,
        y: 0,
        screen_x: x,
        screen_y: 0,
        width: 40,
        height: 1,
        shift: false,
        alt: false,
        ctrl: false,
        wheel_delta: None,
        click_count: None,
    }
}

#[test]
fn mouse_click_offsets_by_custom_prompt_width() {
    let mut input = Input::new(InputOptions { prompt: Some("Search: ".into()), ..Default::default() });
    input.set_value("abcdef");
    input.render(40);
    input.handle_mouse(&left_press(8 + 2));
    assert_eq!(input.cursor, 2);
    input.handle_mouse(&left_press(8));
    assert_eq!(input.cursor, 0);
}
