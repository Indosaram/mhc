//! Port of senpi `packages/coding-agent/src/core/session-resident-store-size.ts`.

/// What a resident store holds in memory right now: entry count and summed UTF-8 length.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResidentStoreSize {
    pub entries: usize,
    pub approx_bytes: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResidentSizeStats {
    pub blob_bytes: usize,
    pub evicted_count: usize,
    pub evicted_bytes: usize,
}

/// Incremental byte accounting of a resident string store: updated per string, never by re-serializing.
#[derive(Debug, Clone, Default)]
pub struct ResidentSizeAccounting {
    bytes: usize,
    evicted_count: usize,
    evicted_bytes: usize,
}

impl ResidentSizeAccounting {
    pub fn resident_bytes(&self) -> usize {
        self.bytes
    }

    pub fn added(&mut self, text: &str) {
        self.bytes += text.len();
    }

    pub fn removed(&mut self, text: &str) {
        self.bytes = self.bytes.saturating_sub(text.len());
    }

    /// A string left memory for its blob file.
    pub fn evicted(&mut self, text: &str) {
        self.evicted_count += 1;
        self.evicted_bytes += text.len();
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn stats(&self) -> ResidentSizeStats {
        ResidentSizeStats {
            blob_bytes: self.bytes,
            evicted_count: self.evicted_count,
            evicted_bytes: self.evicted_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_utf8_bytes_incrementally() {
        let mut accounting = ResidentSizeAccounting::default();
        accounting.added("ab");
        accounting.added("\u{d55c}");
        assert_eq!(accounting.resident_bytes(), 5);
        accounting.removed("ab");
        accounting.evicted("\u{d55c}");
        assert_eq!(
            accounting.stats(),
            ResidentSizeStats {
                blob_bytes: 3,
                evicted_count: 1,
                evicted_bytes: 3
            }
        );
        accounting.reset();
        assert_eq!(accounting.stats(), ResidentSizeStats::default());
    }
}
