//! Host identity: the port of `node:os`'s `hostname()`.

use std::env;
use std::fs;

/// Best-effort host name, matching Node's `os.hostname()` fallback behaviour.
///
/// Lock records store this value so a contender can tell "same machine" from
/// "another machine"; an empty result degrades to `unknown` rather than an
/// empty string, because lock-record validation rejects empty host names.
pub fn hostname() -> String {
    if let Ok(name) = env::var("HOSTNAME") {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Ok(name) = env::var("COMPUTERNAME") {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    for candidate in ["/etc/hostname", "/proc/sys/kernel/hostname"] {
        if let Ok(contents) = fs::read_to_string(candidate) {
            let trimmed = contents.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    "unknown".to_string()
}
