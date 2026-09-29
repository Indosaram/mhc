//! Port of senpi `packages/tui/src/native-modifiers.ts`.

pub use crate::native_platform::ModifierKey;
use crate::native_platform::get_native_platform_helper;

pub fn is_native_modifier_pressed(key: ModifierKey) -> bool {
    get_native_platform_helper()
        .and_then(|helper| helper.is_modifier_pressed(key))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_not_pressed_without_a_native_helper() {
        assert!(!is_native_modifier_pressed(ModifierKey::Shift));
    }
}
