//! Port of senpi `packages/coding-agent/src/core/dynamic-prompt/identity.ts`.

use crate::config::app_name;

/// `buildIdentitySection`.
pub fn build_identity_section() -> String {
    format!("You are {}, a coding agent. Your work should be indistinguishable from a careful senior engineer's.", app_name())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identity_names_the_app_and_keeps_the_neutral_stance() {
        let section = build_identity_section();
        assert!(section.starts_with(&format!("You are {}, a coding agent.", app_name())));
        assert!(section.ends_with("indistinguishable from a careful senior engineer's."));
    }
}
