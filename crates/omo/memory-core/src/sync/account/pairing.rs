use crate::sync::{
    crypto::{parse::Principal, signatures::*},
    recovery::RecoveryError,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;
pub struct PairingParty<'a> {
    pub principal: &'a Principal,
    pub nonce: &'a str,
}
pub fn new_pairing_nonce() -> Result<String, RecoveryError> {
    {
        use aws_lc_rs::rand::{SecureRandom, SystemRandom};
        let mut bytes = [0; 16];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| RecoveryError::Crypto)?;
        Ok(URL_SAFE_NO_PAD.encode(bytes))
    }
}
pub fn approval_commitment(
    account: &str,
    joining: &PairingParty<'_>,
) -> Result<String, RecoveryError> {
    tagged_hash("omo-sync/v1/approval-commitment", &json!({"account":account,"sealing_public_key":joining.principal.sealing_public_key,"signing_public_key":joining.principal.signing_public_key,"nonce":joining.nonce})).ok_or(RecoveryError::Crypto)
}
pub fn approval_code(
    account: &str,
    mut parties: [PairingParty<'_>; 2],
) -> Result<String, RecoveryError> {
    let left =
        decode_b64(&parties[0].principal.signing_public_key, None).ok_or(RecoveryError::Crypto)?;
    let right =
        decode_b64(&parties[1].principal.signing_public_key, None).ok_or(RecoveryError::Crypto)?;
    if left > right {
        parties.swap(0, 1);
    }
    let devices: Vec<_> = parties.iter().map(|party| json!({"sealing_public_key":party.principal.sealing_public_key,"signing_public_key":party.principal.signing_public_key,"nonce":party.nonce})).collect();
    let hash = tagged_hash(
        "omo-sync/v1/approval-code",
        &json!({"account":account,"devices":devices}),
    )
    .ok_or(RecoveryError::Crypto)?;
    let digest = decode_b64(&hash, Some(32)).ok_or(RecoveryError::Crypto)?;
    Ok(format!(
        "{:06}",
        u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 1_000_000
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn party(seed: u8) -> Principal {
        Principal {
            kind: "device".into(),
            id: format!("d{seed}"),
            signing_public_key: URL_SAFE_NO_PAD.encode(
                (0..32)
                    .map(|i| seed.wrapping_mul(7).wrapping_add(i))
                    .collect::<Vec<_>>(),
            ),
            sealing_public_key: URL_SAFE_NO_PAD.encode(
                (0..32)
                    .map(|i| seed.wrapping_mul(7).wrapping_add(i).wrapping_add(100))
                    .collect::<Vec<_>>(),
            ),
        }
    }
    #[test]
    fn both_screens_match_independent_framing() {
        let a = party(1);
        let b = party(200);
        let na = URL_SAFE_NO_PAD.encode([5; 16]);
        let nb = URL_SAFE_NO_PAD.encode([9; 16]);
        let code = approval_code(
            "acct",
            [
                PairingParty {
                    principal: &a,
                    nonce: &na,
                },
                PairingParty {
                    principal: &b,
                    nonce: &nb,
                },
            ],
        )
        .expect("code");
        assert_eq!(
            approval_code(
                "acct",
                [
                    PairingParty {
                        principal: &b,
                        nonce: &nb
                    },
                    PairingParty {
                        principal: &a,
                        nonce: &na
                    }
                ]
            )
            .expect("code"),
            code
        );
        let mut ordered = [(&a, &na), (&b, &nb)];
        ordered.sort_by_key(|(p, _)| URL_SAFE_NO_PAD.decode(&p.signing_public_key).expect("key"));
        let devices:Vec<_>=ordered.iter().map(|(p,n)|json!({"sealing_public_key":p.sealing_public_key,"signing_public_key":p.signing_public_key,"nonce":n})).collect();
        let mut framed = b"omo-sync/v1/approval-code\0".to_vec();
        framed.extend(canonical(&json!({"account":"acct","devices":devices})).expect("canonical"));
        let digest = sha256(&framed);
        assert_eq!(
            code,
            format!(
                "{:06}",
                u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 1_000_000
            )
        );
    }
    #[test]
    fn substitutions_change_code_and_commitment() {
        let a = party(1);
        let b = party(200);
        let c = party(201);
        let code = approval_code(
            "acct",
            [
                PairingParty {
                    principal: &a,
                    nonce: "BQ",
                },
                PairingParty {
                    principal: &b,
                    nonce: "CQ",
                },
            ],
        )
        .expect("code");
        assert_ne!(
            code,
            approval_code(
                "acct",
                [
                    PairingParty {
                        principal: &a,
                        nonce: "BQ"
                    },
                    PairingParty {
                        principal: &c,
                        nonce: "CQ"
                    }
                ]
            )
            .expect("code")
        );
        assert_ne!(
            code,
            approval_code(
                "acct",
                [
                    PairingParty {
                        principal: &a,
                        nonce: "BQ"
                    },
                    PairingParty {
                        principal: &b,
                        nonce: "Cg"
                    }
                ]
            )
            .expect("code")
        );
        let commitment = approval_commitment(
            "acct",
            &PairingParty {
                principal: &a,
                nonce: "BQ",
            },
        )
        .expect("commitment");
        let mut bytes = b"omo-sync/v1/approval-commitment\0".to_vec();
        bytes.extend(canonical(&json!({"account":"acct","sealing_public_key":a.sealing_public_key,"signing_public_key":a.signing_public_key,"nonce":"BQ"})).expect("canonical"));
        assert_eq!(commitment, sha256_b64(&bytes));
        assert_ne!(
            commitment,
            approval_commitment(
                "acct",
                &PairingParty {
                    principal: &a,
                    nonce: "CQ"
                }
            )
            .expect("commitment")
        );
    }
}
