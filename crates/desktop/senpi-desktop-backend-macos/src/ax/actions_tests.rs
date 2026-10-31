use std::cell::RefCell;

use objc2_application_services::AXError;
use objc2_core_foundation::{CFString, CFType};
use senpi_desktop_core::error::{DesktopError, ErrorCode};

use super::{focus, perform_requested, request_result, write_value, AxRequests};

/// An element that answers every request with `reply` and records what it
/// was asked.
struct ScriptedElement {
    reply: AXError,
    requests: RefCell<Vec<String>>,
}

impl ScriptedElement {
    fn replying(reply: AXError) -> Self {
        Self {
            reply,
            requests: RefCell::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<String> {
        self.requests.borrow().clone()
    }
}

impl AxRequests for ScriptedElement {
    fn send_action(&self, action: &CFString) -> AXError {
        self.requests.borrow_mut().push(format!("perform {action}"));
        self.reply
    }

    fn write_attribute(&self, attribute: &CFString, _value: &CFType) -> AXError {
        self.requests.borrow_mut().push(format!("set {attribute}"));
        self.reply
    }
}

fn assert_unconfirmed(error: &DesktopError, request: &str) {
    assert_eq!(error.code, ErrorCode::AxUnconfirmed, "{error}");
    assert!(error.message.contains(request), "{}", error.message);
    assert!(error.message.contains("could not be confirmed"), "{}", error.message);
    assert!(error.message.contains("before retrying"), "{}", error.message);
}

#[test]
fn only_cannot_complete_reports_the_request_as_unconfirmed() {
    // Given
    let refusals = [
        AXError::Failure,
        AXError::IllegalArgument,
        AXError::InvalidUIElement,
        AXError::AttributeUnsupported,
        AXError::ActionUnsupported,
        AXError::NotImplemented,
        AXError::APIDisabled,
    ];
    // When
    let unconfirmed = request_result(AXError::CannotComplete, "AX action 'AXPress'", "failed").unwrap_err();
    let refused: Vec<ErrorCode> = refusals
        .into_iter()
        .map(|error| request_result(error, "AX action 'AXPress'", "failed").unwrap_err().code)
        .collect();
    // Then
    assert_unconfirmed(&unconfirmed, "AX action 'AXPress'");
    assert_eq!(refused, vec![ErrorCode::AxFailed; refusals.len()]);
    assert!(request_result(AXError::Success, "AX action 'AXPress'", "failed").is_ok());
}

#[test]
fn a_perform_without_a_reply_is_unconfirmed() {
    // Given
    let element = ScriptedElement::replying(AXError::CannotComplete);
    // When
    let error = perform_requested(&element, "press").unwrap_err();
    // Then
    assert_unconfirmed(&error, "AX action 'AXPress'");
    assert_eq!(element.requests(), ["perform AXPress"]);
}

#[test]
fn a_refused_perform_stays_failed() {
    // Given
    let element = ScriptedElement::replying(AXError::IllegalArgument);
    // When
    let error = perform_requested(&element, "AXShowMenu").unwrap_err();
    // Then
    assert_eq!(error.code, ErrorCode::AxFailed, "{error}");
    assert!(error.message.contains("AX action 'AXShowMenu' failed"), "{}", error.message);
    assert!(perform_requested(&ScriptedElement::replying(AXError::Success), "press").is_ok());
}

#[test]
fn a_value_write_without_a_reply_is_unconfirmed() {
    // Given
    let element = ScriptedElement::replying(AXError::CannotComplete);
    // When
    let error = write_value(&element, "hello").unwrap_err();
    // Then
    assert_unconfirmed(&error, "setting AXValue");
    assert_eq!(element.requests(), ["set AXValue"]);
}

#[test]
fn a_refused_value_write_stays_failed() {
    // Given
    let element = ScriptedElement::replying(AXError::AttributeUnsupported);
    // When
    let error = write_value(&element, "hello").unwrap_err();
    // Then
    assert_eq!(error.code, ErrorCode::AxFailed, "{error}");
    assert!(error.message.contains("AXValue is not settable"), "{}", error.message);
    assert!(write_value(&ScriptedElement::replying(AXError::Success), "hello").is_ok());
}

#[test]
fn a_focus_write_without_a_reply_is_unconfirmed() {
    // Given
    let element = ScriptedElement::replying(AXError::CannotComplete);
    // When
    let error = focus(&element).unwrap_err();
    // Then
    assert_unconfirmed(&error, "setting AXFocused=true");
    assert_eq!(element.requests(), ["set AXFocused"]);
}

#[test]
fn a_refused_focus_write_stays_failed() {
    // Given
    let element = ScriptedElement::replying(AXError::IllegalArgument);
    // When
    let error = focus(&element).unwrap_err();
    // Then
    assert_eq!(error.code, ErrorCode::AxFailed, "{error}");
    assert!(error.message.contains("setting AXFocused=true failed"), "{}", error.message);
    assert!(focus(&ScriptedElement::replying(AXError::Success)).is_ok());
}
