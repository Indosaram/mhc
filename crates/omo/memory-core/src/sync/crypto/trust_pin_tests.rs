use super::{testing::*, *};
#[test]
fn pin_is_write_once_and_corruption_never_resets_trust() {
    let f = fixtures();
    let state = accepted(&f);
    let candidate = state.pin().expect("verified candidate");
    let dir = tempfile::tempdir().expect("state dir");
    assert!(matches!(
        load_trust_anchor(dir.path(), "encoding-account").expect("load"),
        PinLoad::Refused {
            reason: "no_pin",
            ..
        }
    ));
    let first = pin_trust_anchor(dir.path(), "encoding-account", candidate).expect("pin");
    assert_eq!(first.genesis_digest(), candidate.genesis_digest());
    assert_eq!(
        pin_trust_anchor(dir.path(), "encoding-account", candidate)
            .expect("idempotent pin")
            .genesis_digest(),
        first.genesis_digest()
    );
    let mut conflicting = candidate.clone();
    conflicting.genesis_digest = signatures::sha256_b64(b"different root");
    assert!(matches!(
        pin_trust_anchor(dir.path(), "encoding-account", &conflicting),
        Err(TrustPinError::Refused(TrustPinProblem::PinConflict))
    ));
    let path = dir
        .path()
        .join("trust-anchor")
        .join(signatures::sha256_b64(b"encoding-account"))
        .join("pin.json");
    std::fs::write(&path, b"{").expect("corrupt fixture");
    assert!(matches!(
        load_trust_anchor(dir.path(), "encoding-account").expect("load corrupt"),
        PinLoad::Refused {
            reason: "corrupt_pin",
            next: ANCHOR_NEXT_STEP
        }
    ));
    assert!(matches!(
        pin_trust_anchor(dir.path(), "encoding-account", candidate),
        Err(TrustPinError::Refused(TrustPinProblem::CorruptPin))
    ));
    assert_eq!(std::fs::read(path).expect("unchanged"), b"{");
}
