use thiserror::Error;

pub const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
pub const DEFAULT_MAX_CBOR_BYTE_LENGTH: usize = 16 * 1024 * 1024;
pub const DEFAULT_MAX_CBOR_CONTAINER_LENGTH: usize = 1_000_000;
pub const DEFAULT_MAX_CBOR_DEPTH: usize = 64;

#[derive(Debug, Error, PartialEq, Eq)]
#[error("{0}")]
pub struct CborError(pub String);

#[derive(Clone, Copy, Debug)]
pub struct CborOptions {
    pub max_byte_length: usize,
    pub max_container_length: usize,
    pub max_depth: usize,
}

impl Default for CborOptions {
    fn default() -> Self {
        Self {
            max_byte_length: DEFAULT_MAX_CBOR_BYTE_LENGTH,
            max_container_length: DEFAULT_MAX_CBOR_CONTAINER_LENGTH,
            max_depth: DEFAULT_MAX_CBOR_DEPTH,
        }
    }
}

impl CborOptions {
    pub fn resolve(self) -> Result<Self, CborError> {
        for (name, value, maximum) in [
            ("maxByteLength", self.max_byte_length, u32::MAX as usize),
            (
                "maxContainerLength",
                self.max_container_length,
                u32::MAX as usize,
            ),
            ("maxDepth", self.max_depth, 512),
        ] {
            if value > maximum {
                return Err(CborError(format!(
                    "{name} must be an integer between 0 and {maximum}"
                )));
            }
        }
        Ok(self)
    }
}
