//! Minisign verification of downloaded release archives against the key embedded at build time.

use std::io::Read;
use std::path::Path;

include!(concat!(env!("OUT_DIR"), "/release_pubkey.rs"));

/// First release that ships `.minisig` files. Targets at or after it must carry a valid signature.
pub const FIRST_SIGNED_VERSION: &str = "2026.10.24.1";

#[derive(Debug, PartialEq, Eq)]
pub enum SignatureError {
    /// This build embeds no public key, so it can never trust an update.
    NoTrustRoot,
    /// The release is at or after `FIRST_SIGNED_VERSION` but publishes no signature.
    MissingSignature,
    /// The release predates signatures and `--allow-unsigned` was not given.
    UnsignedNotAllowed,
    /// The signature, key or archive could not be used, or the signature does not match.
    Invalid(String),
}

impl std::fmt::Display for SignatureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoTrustRoot => f.write_str("this build cannot verify updates; install a release build"),
            Self::MissingSignature => f.write_str("release has no signature (.minisig); refusing to install an unsigned update"),
            Self::UnsignedNotAllowed => f.write_str("release predates signed updates; rerun with --allow-unsigned to install it without a signature"),
            Self::Invalid(reason) => write!(f, "signature verification failed: {reason}"),
        }
    }
}

impl std::error::Error for SignatureError {}

/// The `mhc --version --verbose` identity lines: build channel and the embedded release key.
pub fn build_info() -> String {
    format!("channel: {RELEASE_CHANNEL}\nrelease-key: {}\n", RELEASE_PUBKEY.unwrap_or("none"))
}

fn requires_signature(version: &str) -> bool {
    super::release::compare_versions(version, FIRST_SIGNED_VERSION)
        .map_or(true, |order| order != std::cmp::Ordering::Less)
}

/// Decides whether `archive` may be installed. `pubkey` is the base64 minisign public key (the trust root).
pub fn verify_archive(
    pubkey: Option<&str>,
    archive: &Path,
    signature: Option<&str>,
    version: &str,
    allow_unsigned: bool,
) -> Result<(), SignatureError> {
    let pubkey = pubkey.ok_or(SignatureError::NoTrustRoot)?;
    let Some(signature) = signature else {
        return if requires_signature(version) {
            Err(SignatureError::MissingSignature)
        } else if allow_unsigned {
            Ok(())
        } else {
            Err(SignatureError::UnsignedNotAllowed)
        };
    };
    let invalid = |error: &dyn std::fmt::Display| SignatureError::Invalid(error.to_string());
    let key = minisign_verify::PublicKey::from_base64(pubkey).map_err(|e| invalid(&e))?;
    let signature = minisign_verify::Signature::decode(signature).map_err(|e| invalid(&e))?;
    let mut verifier = key.verify_stream(&signature).map_err(|e| invalid(&e))?;
    let mut file = std::fs::File::open(archive).map_err(|e| invalid(&e))?;
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|e| invalid(&e))?;
        if read == 0 {
            break;
        }
        verifier.update(&buffer[..read]);
    }
    verifier.finalize().map_err(|e| invalid(&e))
}
