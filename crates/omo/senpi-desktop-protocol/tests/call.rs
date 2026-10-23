use senpi_desktop_protocol::{ComputerCallStep, is_read_only_computer_call};

fn rejection_reason(chain: &[ComputerCallStep]) -> &'static str {
    match is_read_only_computer_call(chain) {
        Ok(_) => panic!("expected the chain to be rejected"),
        Err(err) => err.reason.as_str(),
    }
}

#[test]
fn returns_true_when_a_window_screenshot_chain_is_inspection_only() {
    let chain = vec![
        ComputerCallStep {
            method: "window".to_string(),
            args: None,
        },
        ComputerCallStep {
            method: "screenshot".to_string(),
            args: None,
        },
    ];
    assert!(is_read_only_computer_call(&chain).unwrap());
}

#[test]
fn returns_false_when_a_window_click_chain_mutates() {
    let chain = vec![
        ComputerCallStep {
            method: "window".to_string(),
            args: None,
        },
        ComputerCallStep {
            method: "click".to_string(),
            args: None,
        },
    ];
    assert!(!is_read_only_computer_call(&chain).unwrap());
}

#[test]
fn returns_false_when_clipboard_write_is_the_only_step() {
    let chain = vec![ComputerCallStep {
        method: "clipboard.write".to_string(),
        args: None,
    }];
    assert!(!is_read_only_computer_call(&chain).unwrap());
}

#[test]
fn returns_false_when_a_ref_press_chain_mutates() {
    let chain = vec![
        ComputerCallStep {
            method: "ref".to_string(),
            args: None,
        },
        ComputerCallStep {
            method: "press".to_string(),
            args: None,
        },
    ];
    assert!(!is_read_only_computer_call(&chain).unwrap());
}

#[test]
fn throws_too_long_when_a_call_chain_is_longer_than_two_steps() {
    let chain = vec![
        ComputerCallStep {
            method: "window".to_string(),
            args: None,
        },
        ComputerCallStep {
            method: "screenshot".to_string(),
            args: None,
        },
        ComputerCallStep {
            method: "click".to_string(),
            args: None,
        },
    ];
    assert_eq!(rejection_reason(&chain), "tooLong");
}

#[test]
fn throws_empty_when_the_chain_has_no_steps() {
    let chain: Vec<ComputerCallStep> = Vec::new();
    assert_eq!(rejection_reason(&chain), "empty");
}

#[test]
fn rejects_an_unknown_reset_chain_as_unknown_instead_of_classifying_it_read() {
    let chain = vec![ComputerCallStep {
        method: "reset".to_string(),
        args: None,
    }];
    assert_eq!(rejection_reason(&chain), "unknownMethod");
}

#[test]
fn rejects_an_unknown_handle_method_as_unknown_instead_of_classifying_it_read() {
    let chain = vec![
        ComputerCallStep {
            method: "window".to_string(),
            args: None,
        },
        ComputerCallStep {
            method: "reset".to_string(),
            args: None,
        },
    ];
    assert_eq!(rejection_reason(&chain), "unknownMethod");
}

#[test]
fn rejects_a_chained_call_on_a_root_that_returns_no_handle() {
    let chain = vec![
        ComputerCallStep {
            method: "screenshot".to_string(),
            args: None,
        },
        ComputerCallStep {
            method: "click".to_string(),
            args: None,
        },
    ];
    assert_eq!(rejection_reason(&chain), "notChainable");
}
