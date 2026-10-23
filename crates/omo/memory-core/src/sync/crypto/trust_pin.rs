use super::{
    anchor::*,
    envelope::{exact, integer},
    signatures::*,
};
use crate::locks::{ProcessLiveness, get_pid_liveness};
use aws_lc_rs::rand::{SecureRandom, SystemRandom};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};
pub enum PinLoad {
    Loaded {
        anchor: TrustAnchor,
    },
    Refused {
        reason: &'static str,
        next: &'static str,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustPinProblem {
    NotVerified,
    PinConflict,
    CorruptPin,
}
impl TrustPinProblem {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotVerified => "not_verified",
            Self::PinConflict => "pin_conflict",
            Self::CorruptPin => "corrupt_pin",
        }
    }
}
#[derive(Debug)]
pub enum TrustPinError {
    Refused(TrustPinProblem),
    Io(io::Error),
    Entropy,
}
impl TrustPinError {
    pub const fn next(&self) -> &'static str {
        ANCHOR_NEXT_STEP
    }
}
impl std::fmt::Display for TrustPinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "trust pin refused: {self:?}; {ANCHOR_NEXT_STEP}")
    }
}
impl std::error::Error for TrustPinError {}
impl From<io::Error> for TrustPinError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
fn root(state: &Path) -> PathBuf {
    state.join("trust-anchor")
}
fn target(state: &Path, account: &str) -> PathBuf {
    root(state).join(sha256_b64(account.as_bytes()))
}
/// fsync a directory so a rename or create inside it survives a crash (no-op on Windows).
fn sync_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}
fn random_suffix() -> Result<String, TrustPinError> {
    let mut bytes = [0; 6];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| TrustPinError::Entropy)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn clean_stale_candidates(state: &Path, account: &str) -> io::Result<()> {
    let names = match fs::read_dir(root(state)) {
        Ok(v) => v,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let prefix = format!("{}.", sha256_b64(account.as_bytes()));
    for entry in names {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(&prefix) || !name.ends_with(".tmp") {
            continue;
        }
        let Some(pid) = name[prefix.len()..]
            .split('.')
            .next()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if get_pid_liveness(pid) == ProcessLiveness::Dead {
            fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}
fn parse_pin(raw: &[u8], account: &str) -> Option<TrustAnchor> {
    let value: Value = serde_json::from_slice(raw).ok()?;
    if !exact(
        &value,
        &[
            "version",
            "account",
            "genesis_digest",
            "pinned_by",
            "pinned_at",
        ],
    ) || value["version"] != 1
        || value["account"] != account
    {
        return None;
    }
    let digest = value["genesis_digest"].as_str()?;
    decode_b64(digest, Some(32))?;
    match value["pinned_by"].as_str()? {
        "pairing" | "recovery" | "creator" => {}
        _ => return None,
    }
    let at = &value["pinned_at"];
    if !exact(at, &["seq", "keyring_link"]) {
        return None;
    }
    integer(&at["seq"], 1)?;
    decode_b64(at["keyring_link"].as_str()?, Some(32))?;
    Some(TrustAnchor {
        account: account.into(),
        genesis_digest: digest.into(),
    })
}
pub fn load_trust_anchor(state: &Path, account: &str) -> Result<PinLoad, TrustPinError> {
    clean_stale_candidates(state, account)?;
    let raw = match fs::read(target(state, account).join("pin.json")) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let reason = match fs::metadata(target(state, account)) {
                Ok(_) => "corrupt_pin",
                Err(e) if e.kind() == io::ErrorKind::NotFound => "no_pin",
                Err(e) => return Err(e.into()),
            };
            return Ok(PinLoad::Refused {
                reason,
                next: ANCHOR_NEXT_STEP,
            });
        }
        Err(e) => return Err(e.into()),
    };
    Ok(match parse_pin(&raw, account) {
        Some(anchor) => PinLoad::Loaded { anchor },
        None => PinLoad::Refused {
            reason: "corrupt_pin",
            next: ANCHOR_NEXT_STEP,
        },
    })
}
pub fn pin_trust_anchor(
    state: &Path,
    account: &str,
    candidate: &PinCandidate,
) -> Result<TrustAnchor, TrustPinError> {
    if candidate.account != account {
        return Err(TrustPinError::Refused(TrustPinProblem::NotVerified));
    }
    match load_trust_anchor(state, account)? {
        PinLoad::Loaded { anchor } => {
            return if anchor.genesis_digest == candidate.genesis_digest {
                Ok(anchor)
            } else {
                Err(TrustPinError::Refused(TrustPinProblem::PinConflict))
            };
        }
        PinLoad::Refused {
            reason: "corrupt_pin",
            ..
        } => return Err(TrustPinError::Refused(TrustPinProblem::CorruptPin)),
        PinLoad::Refused { .. } => {}
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(root(state))?;
    let target = target(state, account);
    let staging = root(state).join(format!(
        "{}.{}.{}.tmp",
        sha256_b64(account.as_bytes()),
        std::process::id(),
        random_suffix()?
    ));
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    builder.create(&staging)?;
    let result = (|| -> Result<(), TrustPinError> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(staging.join("pin.json"))?;
        let pin = json!({"version":1,"account":account,"genesis_digest":candidate.genesis_digest,"pinned_by":candidate.pinned_by,"pinned_at":{"seq":candidate.seq,"keyring_link":candidate.keyring_link}});
        file.write_all(format!("{pin}\n").as_bytes())?;
        file.sync_all()?;
        // Windows cannot rename the directory while its pin file is open.
        drop(file);
        sync_dir(&staging)?;
        if let Err(error) = fs::rename(&staging, &target) {
            // Losing the publishing rename to a concurrent writer is decided by the pin it published.
            if !matches!(load_trust_anchor(state, account)?, PinLoad::Loaded { .. }) {
                return Err(error.into());
            }
        }
        Ok(())
    })();
    let cleanup = match fs::remove_dir_all(&staging) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(TrustPinError::Io(e)),
    };
    result?;
    cleanup?;
    sync_dir(&root(state))?;
    match load_trust_anchor(state, account)? {
        PinLoad::Loaded { anchor } if anchor.genesis_digest == candidate.genesis_digest => {
            Ok(anchor)
        }
        _ => Err(TrustPinError::Refused(TrustPinProblem::PinConflict)),
    }
}

pub fn forget_trust_anchor(state: &Path, account: &str) -> Result<(), TrustPinError> {
    let source = target(state, account);
    let retired = root(state).join(format!(
        "{}.retired-{}-{}",
        sha256_b64(account.as_bytes()),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| TrustPinError::Io(io::Error::other(e)))?
            .as_millis(),
        random_suffix()?
    ));
    match fs::rename(source, retired) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
