//! Memory that expires after a session's pin (port of `projection-expiry.ts`, #11450).
//!
//! The pinned prompt keeps its bytes (#8470), so a block that was live at the pin instant stays in
//! the prompt until the next repin; the model is told once, in the late `<memory_notice>`, that it no
//! longer holds. Expiry instants are announced through a watermark kept in the pin record, so a
//! resumed or restarted session does not announce the same instant again.
//!
//! Cross-lane dependency: `memory_core::compile::compile::MemoryExpiry { path, expires_at_ms }`
//! (memory-core lane, upstream `MemoryExpiry`).

use memory_core::compile::compile::MemoryExpiry;

pub const MEMORY_EXPIRED_METADATA_TOKEN: &str = "Expired since this prompt was pinned";
const MAX_LISTED_PATHS: usize = 20;

/// The announcement window of `newlyExpired`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExpiryWindow {
    pub pinned_at_ms: f64,
    pub noticed_through_ms: f64,
    pub now_ms: f64,
}

/// Expiries that were live at the pin, passed by `now_ms`, and not yet announced.
pub fn newly_expired(expiries: &[MemoryExpiry], window: ExpiryWindow) -> Vec<MemoryExpiry> {
    let after = window.pinned_at_ms.max(window.noticed_through_ms);
    expiries
        .iter()
        .filter(|expiry| {
            let at = expiry.expires_at_ms as f64;
            at > after && at <= window.now_ms
        })
        .cloned()
        .collect()
}

pub fn render_expired_line(expired: &[MemoryExpiry]) -> String {
    let hidden = expired.len().saturating_sub(MAX_LISTED_PATHS);
    let listed = expired
        .iter()
        .take(MAX_LISTED_PATHS)
        .map(|expiry| expiry.path.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let more = if hidden > 0 {
        format!(", {hidden} more")
    } else {
        String::new()
    };
    format!("- {MEMORY_EXPIRED_METADATA_TOKEN} (no longer true): {listed}{more}.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expiry(path: &str, at: i64) -> MemoryExpiry {
        MemoryExpiry {
            path: path.to_string(),
            expires_at_ms: at,
        }
    }

    #[test]
    fn only_instants_after_the_pin_and_watermark_up_to_now_are_new() {
        let expiries = [
            expiry("system/a.md", 5),
            expiry("system/b.md", 15),
            expiry("system/c.md", 25),
            expiry("system/d.md", 40),
        ];
        let window = ExpiryWindow {
            pinned_at_ms: 10.0,
            noticed_through_ms: 20.0,
            now_ms: 30.0,
        };
        let paths: Vec<_> = newly_expired(&expiries, window)
            .into_iter()
            .map(|e| e.path)
            .collect();
        assert_eq!(paths, ["system/c.md"]);
    }

    #[test]
    fn the_line_lists_twenty_paths_then_counts_the_rest() {
        let expired: Vec<_> = (0..22)
            .map(|i| expiry(&format!("system/{i}.md"), 1))
            .collect();
        let line = render_expired_line(&expired);
        assert!(
            line.starts_with(
                "- Expired since this prompt was pinned (no longer true): system/0.md, "
            )
        );
        assert!(line.contains("system/19.md"));
        assert!(!line.contains("system/20.md"));
        assert!(line.ends_with(", 2 more."));
        assert_eq!(
            render_expired_line(&expired[..1]),
            "- Expired since this prompt was pinned (no longer true): system/0.md."
        );
    }
}
