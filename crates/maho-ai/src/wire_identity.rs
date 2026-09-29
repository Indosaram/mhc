//! Port of senpi packages/ai/src/wire-identity.ts.

use std::sync::RwLock;

const DEFAULT_WIRE_IDENTITY: &str = "senpi";

static WIRE_IDENTITY: RwLock<Option<String>> = RwLock::new(None);

/// Sets the product identity sent on the wire; blank values are ignored.
pub fn set_wire_identity(identity: Option<&str>) {
    let Some(trimmed) = identity.map(crate::utils::js::trim).filter(|v| !v.is_empty()) else { return };
    *WIRE_IDENTITY.write().unwrap_or_else(|p| p.into_inner()) = Some(trimmed.to_owned());
}

pub fn get_wire_identity() -> String {
    WIRE_IDENTITY
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .unwrap_or_else(|| DEFAULT_WIRE_IDENTITY.to_owned())
}

pub fn reset_wire_identity_for_tests() {
    *WIRE_IDENTITY.write().unwrap_or_else(|p| p.into_inner()) = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_trims_and_ignores_blank() {
        reset_wire_identity_for_tests();
        assert_eq!(get_wire_identity(), "senpi");
        set_wire_identity(Some("  "));
        set_wire_identity(None);
        assert_eq!(get_wire_identity(), "senpi");
        set_wire_identity(Some(" maho "));
        assert_eq!(get_wire_identity(), "maho");
        reset_wire_identity_for_tests();
        assert_eq!(get_wire_identity(), "senpi");
    }
}
