//! Release ownership is independent of the backend's foreground/AX scope.
use super::held::{ButtonRoute, Held, HeldButton, KeyRoute};
use super::{cgevent, global, keys};
use crate::release_helper::wire::{Input, Route};
use core_graphics::{event::CGEventFlags, event_source::CGEventSource, geometry::CGPoint};
use senpi_desktop_core::backend::MouseButton;
use senpi_desktop_core::error::{CoreResult, DesktopError};

/// Posts, from the release helper, the release of a press the engine streamed
/// to it, through the route and code the in-process release uses. A button on
/// the HID route is released where the pointer now is, so the user's pointer
/// does not jump back to where the press began.
pub(crate) fn post_helper_release(
    route: Route,
    input: Input,
    text: Option<&str>,
    flags: Option<u64>,
) -> CoreResult<()> {
    let source = cgevent::event_source()?;
    let mut detached = Held::default();
    let flags = CGEventFlags::from_bits_truncate(flags.unwrap_or(0));
    let button = |number: u8| match number {
        0 => Some(MouseButton::Left),
        1 => Some(MouseButton::Right),
        2 => Some(MouseButton::Middle),
        _ => None,
    };
    let held = match (input, route) {
        (Input::Key(code), Route::Hid) => {
            return keys::post_key(
                &source,
                &mut detached,
                code,
                text,
                false,
                flags,
                KeyRoute::Global,
            );
        }
        (Input::Key(code), Route::Keyboard(pid)) => {
            return keys::post_key(
                &source,
                &mut detached,
                code,
                text,
                false,
                flags,
                KeyRoute::Process(pid),
            );
        }
        (Input::Button(number), Route::Hid) => match button(number) {
            Some(button) => Some(HeldButton {
                route: ButtonRoute::Global(pointer_location(&source)?),
                button,
            }),
            None => None,
        },
        (Input::Button(number), Route::Window { pid, wid, x, y }) => {
            button(number).map(|button| HeldButton {
                route: ButtonRoute::Window(pid, wid, CGPoint::new(x, y)),
                button,
            })
        }
        (Input::Key(_), Route::Window { .. }) | (Input::Button(_), Route::Keyboard(_)) => None,
    };
    held.map_or(Ok(()), |held| global::release_button(&source, &held))
}

fn pointer_location(source: &CGEventSource) -> CoreResult<CGPoint> {
    crate::cursor::position(source)?
        .map(|point| CGPoint::new(point.x, point.y))
        .ok_or_else(|| DesktopError::input_failed("the pointer position is unknown"))
}
