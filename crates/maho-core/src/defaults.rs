//! Port of senpi packages/coding-agent/src/core/defaults.ts.

pub const DEFAULT_THINKING_LEVEL: &str = "medium";

pub const THINKING_LEVEL_OPTIONS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_level_is_an_option() {
        assert_eq!(DEFAULT_THINKING_LEVEL, "medium");
        assert!(THINKING_LEVEL_OPTIONS.contains(&DEFAULT_THINKING_LEVEL));
        assert_eq!(THINKING_LEVEL_OPTIONS.len(), 7);
    }
}
