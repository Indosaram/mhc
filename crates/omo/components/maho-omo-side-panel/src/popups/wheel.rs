pub const WHEEL_ROWS: i32 = 3;

pub fn parse_panel_wheel_event(data: &str) -> Option<i32> {
    let fields = data.strip_prefix("\x1b[<")?.strip_suffix('M')?;
    let mut fields = fields.split(';');
    let button = fields.next()?.parse::<u32>().ok()?;
    fields.next()?.parse::<u32>().ok()?;
    fields.next()?.parse::<u32>().ok()?;
    if fields.next().is_some() || button & 64 == 0 { return None; }
    match button & 3 { 0 => Some(-1), 1 => Some(1), _ => None }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_vertical_sgr_wheel_presses_are_consumed() {
        assert_eq!(parse_panel_wheel_event("\x1b[<64;10;20M"), Some(-1));
        assert_eq!(parse_panel_wheel_event("\x1b[<65;10;20M"), Some(1));
        for event in ["\x1b[<64;10;20m", "\x1b[<66;10;20M", "\x1b[<0;10;20M", "\x1b[<64;x;20M"] {
            assert_eq!(parse_panel_wheel_event(event), None);
        }
    }
}
