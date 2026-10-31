//! Private desktop sync channel (port of `desktop-sync/` of omo v5.1.52).
//!
//! The desktop app hands the engine one private duplex stream; there is no endpoint lookup and no
//! alternate account source.

pub mod channel;
pub mod framing;
pub mod protocol;
