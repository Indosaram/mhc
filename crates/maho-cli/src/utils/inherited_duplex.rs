//! Port of `packages/coding-agent/src/utils/inherited-duplex.ts` (senpi#4749).

pub const S_IFMT: u32 = 0o170_000;
pub const S_IFIFO: u32 = 0o010_000;

/// Windows can report FIFO mode bits while `is_fifo` metadata queries return false, so pipes are
/// recognised by mode bits.
pub fn is_fifo_mode(mode: u32) -> bool {
    (mode & S_IFMT) == S_IFIFO
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InheritedChannelFd {
    Socket(u32),
    PipePair(u32, u32),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_fifo_mode_bits() {
        assert!(is_fifo_mode(0o010_600));
        assert!(!is_fifo_mode(0o140_755));
    }
}
