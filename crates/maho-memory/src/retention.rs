//! Retention policy for the MIT memory store: what a bank is allowed to keep.
//!
//! Spec: `docs/cleanroom/specs/memory-retention.md`.

/// Which rows a bank may drop, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub working_days: i64,
    pub episodic_days: i64,
    pub episodic_keep_importance: u64,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            working_days: 30,
            episodic_days: 90,
            // Permille, so the boundary is an exact integer: importance is a 0.0-1.0 score and 900 keeps only rows pinned high.
            episodic_keep_importance: 900,
        }
    }
}

impl RetentionPolicy {
    /// Clamped so a stray setting cannot erase a bank, and so a zero window still keeps a day.
    pub fn clamped(self) -> Self {
        Self {
            working_days: self.working_days.clamp(1, 3650),
            episodic_days: self.episodic_days.clamp(1, 3650),
            episodic_keep_importance: self.episodic_keep_importance.min(1000),
        }
    }
}

pub fn may_drop_working(age_days: i64, importance: f64, policy: &RetentionPolicy) -> bool {
    age_days >= policy.working_days
        && importance_permille(importance) < policy.episodic_keep_importance
}

pub fn may_drop_episodic(age_days: i64, importance: f64, policy: &RetentionPolicy) -> bool {
    age_days >= policy.episodic_days
        && importance_permille(importance) < policy.episodic_keep_importance
}

/// Permille, so the keep/drop boundary is an exact integer instead of a float comparison that can wobble.
pub fn importance_permille(importance: f64) -> u64 {
    if importance.is_nan() || importance <= 0.0 {
        return 0;
    }
    (importance * 1000.0).round().clamp(0.0, 1000.0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_policy_keeps_working_rows_a_month() {
        assert_eq!(RetentionPolicy::default().working_days, 30);
    }

    #[test]
    fn the_default_policy_keeps_episodic_rows_a_quarter() {
        assert_eq!(RetentionPolicy::default().episodic_days, 90);
    }

    #[test]
    fn a_young_working_row_is_kept() {
        assert!(!may_drop_working(29, 0.5, &RetentionPolicy::default()));
    }

    #[test]
    fn an_old_working_row_may_go() {
        assert!(may_drop_working(30, 0.5, &RetentionPolicy::default()));
    }

    #[test]
    fn an_important_working_row_is_kept_forever() {
        assert!(!may_drop_working(9_999, 0.95, &RetentionPolicy::default()));
    }

    #[test]
    fn an_important_episodic_row_is_kept_forever() {
        assert!(!may_drop_episodic(9_999, 0.95, &RetentionPolicy::default()));
    }

    #[test]
    fn an_old_episodic_row_may_go() {
        assert!(may_drop_episodic(90, 0.5, &RetentionPolicy::default()));
    }

    #[test]
    fn a_young_episodic_row_is_kept() {
        assert!(!may_drop_episodic(89, 0.5, &RetentionPolicy::default()));
    }

    #[test]
    fn the_importance_boundary_is_exact() {
        let policy = RetentionPolicy {
            episodic_keep_importance: 900,
            ..RetentionPolicy::default()
        };
        assert!(may_drop_episodic(90, 0.899, &policy));
        assert!(!may_drop_episodic(90, 0.9, &policy));
    }

    #[test]
    fn a_nan_importance_does_not_panic_and_is_treated_as_low() {
        assert_eq!(importance_permille(f64::NAN), 0);
        assert!(may_drop_episodic(90, f64::NAN, &RetentionPolicy::default()));
    }

    #[test]
    fn an_infinite_importance_is_clamped() {
        assert_eq!(importance_permille(f64::INFINITY), 1000);
        assert!(!may_drop_episodic(
            90,
            f64::INFINITY,
            &RetentionPolicy::default()
        ));
    }

    #[test]
    fn a_negative_importance_is_zero() {
        assert_eq!(importance_permille(-1.0), 0);
    }

    #[test]
    fn clamping_bounds_the_window() {
        let policy = RetentionPolicy {
            working_days: 0,
            episodic_days: 100_000,
            episodic_keep_importance: 5_000,
        }
        .clamped();
        assert_eq!(policy.working_days, 1);
        assert_eq!(policy.episodic_days, 3650);
        assert_eq!(policy.episodic_keep_importance, 1000);
    }

    #[test]
    fn clamping_leaves_a_reasonable_policy_alone() {
        let policy = RetentionPolicy::default().clamped();
        assert_eq!(policy, RetentionPolicy::default());
    }

    #[test]
    fn a_zero_day_policy_still_keeps_one_day() {
        let policy = RetentionPolicy {
            working_days: 0,
            ..RetentionPolicy::default()
        }
        .clamped();
        assert!(!may_drop_working(0, 0.1, &policy));
    }

    #[test]
    fn a_pinned_row_at_the_top_importance_survives_any_age() {
        assert!(!may_drop_working(100_000, 1.0, &RetentionPolicy::default()));
    }
}
