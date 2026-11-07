//! AX mutations: named actions, `AXValue` writes, and focus.

use objc2_application_services::{AXError, AXUIElement};
use objc2_core_foundation::{CFBoolean, CFString, CFType};
use senpi_desktop_core::error::{CoreResult, DesktopError};

use super::element::ax_result;

pub(crate) fn perform(element: &AXUIElement, action: &str) -> CoreResult<()> {
    let native = action_name(action);
    let action = CFString::from_str(&native);
    // SAFETY: The retained element and action CFString stay valid for the
    // synchronous AX request.
    let error = unsafe { element.perform_action(&action) };
    ax_result(error, format!("AX action '{native}' failed"))
}

/// The AX requests an element write sends, each answered by the app's
/// `AXError` reply: the seam tests script replies through.
pub(super) trait AxRequests {
    fn send_action(&self, action: &CFString) -> AXError;
    fn write_attribute(&self, attribute: &CFString, value: &CFType) -> AXError;
}

impl AxRequests for AXUIElement {
    fn send_action(&self, action: &CFString) -> AXError {
        // SAFETY: The retained element and action CFString stay valid for the
        // synchronous AX request.
        unsafe { self.perform_action(action) }
    }

    fn write_attribute(&self, attribute: &CFString, value: &CFType) -> AXError {
        // SAFETY: The element, attribute, and value stay retained for the
        // synchronous setter call.
        unsafe { self.set_attribute_value(attribute, value) }
    }
}

/// The outcome of a model-requested AX write, from the app's reply.
/// `CannotComplete` means messaging failed or the app did not reply in time,
/// e.g. while the request runs a modal dialog: the request was made, so its
/// outcome is unknown rather than failed (`AxUnconfirmed`, ported from
/// oh-my-pi `element_action_result`). Every other reply keeps the
/// [`ax_result`] mapping.
pub(super) fn request_result(
    error: AXError,
    request: &str,
    failed: impl Into<String>,
) -> CoreResult<()> {
    if error == AXError::CannotComplete {
        return Err(DesktopError::ax_unconfirmed(format!(
            "{request} was requested, but its outcome could not be confirmed: the app did not reply \
             in time or messaging failed ({error:?}), for example because it opened a modal \
             dialog. It may already have taken effect: observe the window before retrying"
        )));
    }
    ax_result(error, failed)
}

/// Performs a model-requested action (`ax.perform`, presses): a request the
/// app got but did not answer is `AxUnconfirmed`.
pub(super) fn perform_requested(
    element: &(impl AxRequests + ?Sized),
    action: &str,
) -> CoreResult<()> {
    let native = action_name(action);
    let error = element.send_action(&CFString::from_str(&native));
    request_result(
        error,
        &format!("AX action '{native}'"),
        format!("AX action '{native}' failed"),
    )
}

pub(super) fn set_value(element: &AXUIElement, value: &str) -> CoreResult<()> {
    write_value(element, value)
}

/// Writes `AXValue` as a `CFString`.
pub(super) fn write_value(element: &(impl AxRequests + ?Sized), value: &str) -> CoreResult<()> {
    let value = CFString::from_str(value);
    let error = element.write_attribute(&CFString::from_str("AXValue"), &value);
    request_result(
        error,
        "setting AXValue",
        "AXValue is not settable; no typing fallback was attempted",
    )
}

pub(super) fn focus(element: &(impl AxRequests + ?Sized)) -> CoreResult<()> {
    let error = element.write_attribute(&CFString::from_str("AXFocused"), CFBoolean::new(true));
    request_result(
        error,
        "setting AXFocused=true",
        "setting AXFocused=true failed",
    )
}

/// Sets `AXMain` and `AXFocused` to true on a window element: the restore and
/// foreground-preparation half of focus handling.
pub(crate) fn set_window_main_and_focused(element: &AXUIElement) -> CoreResult<()> {
    for attribute in ["AXMain", "AXFocused"] {
        let name = CFString::from_str(attribute);
        // SAFETY: The singleton CFBoolean and retained element stay valid for
        // the synchronous setter call.
        let error = unsafe { element.set_attribute_value(&name, CFBoolean::new(true)) };
        ax_result(error, format!("setting {attribute}=true failed"))?;
    }
    Ok(())
}

/// Writes a boolean attribute.
pub(crate) fn set_bool(element: &AXUIElement, attribute: &str, value: bool) -> CoreResult<()> {
    let name = CFString::from_str(attribute);
    // SAFETY: The singleton CFBoolean and retained element stay valid for the
    // synchronous setter call.
    let error = unsafe { element.set_attribute_value(&name, CFBoolean::new(value)) };
    ax_result(error, format!("setting {attribute}={value} failed"))
}

/// Model-facing action names (`press`, `show_menu`) -> native `AX*` names.
pub(super) fn action_name(action: &str) -> String {
    match action.trim().to_ascii_lowercase().as_str() {
        "press" => "AXPress".to_string(),
        "raise" => "AXRaise".to_string(),
        "showmenu" | "show_menu" => "AXShowMenu".to_string(),
        _ if action.starts_with("AX") => action.to_string(),
        _ => format!("AX{action}"),
    }
}

#[cfg(test)]
#[path = "actions_tests.rs"]
mod tests;
