//! Validation of a reflection run's output before it is merged.
//!
//! Spec: `docs/cleanroom/specs/memory-reflection-completion.md`.

use crate::frontmatter::split_frontmatter;
use crate::reflection_machine::DreamOrigin;

/// Whether a pressure dream actually relieved pressure.
///
/// Only `Pressure` runs have a target; every other origin passes regardless of size, because a manual
/// or idle run was never asked to shrink the corpus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DreamTokenBudgetValidation {
    Valid {
        total_tokens: usize,
        target_tokens: usize,
    },
    BudgetNotMet {
        total_tokens: usize,
        target_tokens: usize,
        detail: String,
    },
}

pub fn validate_dream_token_budget(
    origin: DreamOrigin,
    total_tokens: usize,
    target_tokens: usize,
) -> DreamTokenBudgetValidation {
    if origin != DreamOrigin::Pressure || total_tokens < target_tokens {
        DreamTokenBudgetValidation::Valid {
            total_tokens,
            target_tokens,
        }
    } else {
        DreamTokenBudgetValidation::BudgetNotMet {
            total_tokens,
            target_tokens,
            detail: format!(
                "Committed system/ estimate is {total_tokens} tokens; pressure dream target is below {target_tokens} tokens"
            ),
        }
    }
}

pub fn frontmatter_block(content: &str) -> Option<String> {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    split_frontmatter(&normalized).map(|(header, _)| header)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pressure_run_below_its_target_is_valid() {
        let validation = validate_dream_token_budget(DreamOrigin::Pressure, 900, 1000);
        assert_eq!(
            validation,
            DreamTokenBudgetValidation::Valid {
                total_tokens: 900,
                target_tokens: 1000,
            }
        );
    }

    #[test]
    fn a_pressure_run_at_its_target_is_not_met() {
        let validation = validate_dream_token_budget(DreamOrigin::Pressure, 1000, 1000);
        assert!(matches!(
            validation,
            DreamTokenBudgetValidation::BudgetNotMet { .. }
        ));
    }

    #[test]
    fn a_pressure_run_above_its_target_is_not_met() {
        let validation = validate_dream_token_budget(DreamOrigin::Pressure, 1500, 1000);
        assert!(matches!(
            validation,
            DreamTokenBudgetValidation::BudgetNotMet { .. }
        ));
    }

    #[test]
    fn the_detail_names_both_numbers() {
        let DreamTokenBudgetValidation::BudgetNotMet { detail, .. } =
            validate_dream_token_budget(DreamOrigin::Pressure, 1500, 1000)
        else {
            panic!("expected a budget failure");
        };
        assert!(detail.contains("1500"), "{detail}");
        assert!(detail.contains("1000"), "{detail}");
    }

    #[test]
    fn a_manual_run_is_never_measured() {
        let validation = validate_dream_token_budget(DreamOrigin::Manual, 99_999, 1);
        assert!(matches!(
            validation,
            DreamTokenBudgetValidation::Valid { .. }
        ));
    }

    #[test]
    fn an_idle_run_is_never_measured() {
        let validation = validate_dream_token_budget(DreamOrigin::Idle, 99_999, 1);
        assert!(matches!(
            validation,
            DreamTokenBudgetValidation::Valid { .. }
        ));
    }

    #[test]
    fn a_shutdown_run_is_never_measured() {
        let validation = validate_dream_token_budget(DreamOrigin::Shutdown, 99_999, 1);
        assert!(matches!(
            validation,
            DreamTokenBudgetValidation::Valid { .. }
        ));
    }

    #[test]
    fn a_document_without_frontmatter_has_no_block() {
        assert_eq!(frontmatter_block("just a body\n"), None);
    }

    #[test]
    fn a_frontmatter_block_is_returned() {
        let block = frontmatter_block("---\ntitle: x\n---\nbody\n");
        assert_eq!(block.as_deref(), Some("title: x"));
    }

    #[test]
    fn an_empty_frontmatter_block_is_returned() {
        let block = frontmatter_block("---\n---\nbody\n");
        assert_eq!(block.as_deref(), Some(""));
    }

    #[test]
    fn crlf_frontmatter_matches_its_lf_twin() {
        let lf = frontmatter_block("---\ntitle: x\n---\nbody\n");
        let crlf = frontmatter_block("---\r\ntitle: x\r\n---\r\nbody\r\n");
        assert_eq!(lf, crlf);
    }

    #[test]
    fn a_lone_carriage_return_normalizes_too() {
        let lf = frontmatter_block("---\ntitle: x\n---\nbody\n");
        let cr = frontmatter_block("---\rtitle: x\r---\rbody\r");
        assert_eq!(lf, cr);
    }

    #[test]
    fn only_the_header_is_returned() {
        let block = frontmatter_block("---\ntitle: x\n---\nsecret body\n").unwrap();
        assert!(!block.contains("secret body"), "{block}");
    }

    #[test]
    fn a_body_only_edit_keeps_the_same_block() {
        let before = frontmatter_block("---\ntitle: x\n---\nold body\n");
        let after = frontmatter_block("---\ntitle: x\n---\nnew body\n");
        assert_eq!(before, after);
    }

    #[test]
    fn a_frontmatter_edit_changes_the_block() {
        let before = frontmatter_block("---\ntitle: x\n---\nbody\n");
        let after = frontmatter_block("---\ntitle: y\n---\nbody\n");
        assert_ne!(before, after);
    }

    #[test]
    fn an_unterminated_block_has_no_header() {
        assert_eq!(frontmatter_block("---\ntitle: x\nbody\n"), None);
    }
}
