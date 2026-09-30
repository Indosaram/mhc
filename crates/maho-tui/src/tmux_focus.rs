//! Port of senpi `packages/tui/src/tmux-focus.ts`.

pub const ENABLE_FOCUS_REPORTING: &str = "\x1b[?1004h";
pub const DISABLE_FOCUS_REPORTING: &str = "\x1b[?1004l";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusEvent {
    In,
    Out,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxFocusEvent {
    pub event: Option<FocusEvent>,
    pub data: String,
}

pub fn consume_tmux_focus_event(data: &str) -> TmuxFocusEvent {
    let focus_in = data.find("\x1b[I");
    let focus_out = data.find("\x1b[O");
    let index = match (focus_in, focus_out) {
        (None, None) => {
            return TmuxFocusEvent {
                event: None,
                data: data.to_string(),
            };
        }
        (Some(i), None) => i,
        (None, Some(o)) => o,
        (Some(i), Some(o)) => i.min(o),
    };
    TmuxFocusEvent {
        event: Some(if Some(index) == focus_in {
            FocusEvent::In
        } else {
            FocusEvent::Out
        }),
        data: format!("{}{}", &data[..index], &data[index + 3..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(event: Option<FocusEvent>, data: &str) -> TmuxFocusEvent {
        TmuxFocusEvent {
            event,
            data: data.to_string(),
        }
    }

    #[test]
    fn extracts_focus_transitions_without_swallowing_adjacent_input() {
        assert_eq!(
            consume_tmux_focus_event("\x1b[I"),
            ev(Some(FocusEvent::In), "")
        );
        assert_eq!(
            consume_tmux_focus_event("\x1b[O"),
            ev(Some(FocusEvent::Out), "")
        );
        assert_eq!(
            consume_tmux_focus_event("\x1b[Ihello"),
            ev(Some(FocusEvent::In), "hello")
        );
        assert_eq!(consume_tmux_focus_event("hello"), ev(None, "hello"));
    }
}
