//! Port of senpi packages/ai/src/auth/pool/login-id.ts.

use crate::auth::types::Credential;

/// Marks a sign-in write with a fresh random identifier, independent of token material.
pub fn stamp_login(mut credential: Credential) -> Credential {
    let extra = match &mut credential {
        Credential::ApiKey(credential) => &mut credential.extra,
        Credential::OAuth(credential) => &mut credential.extra,
    };
    extra.insert("loginId".into(), uuid::Uuid::new_v4().to_string().into());
    credential
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::types::{ApiKeyCredential, OAuthCredential};

    #[test]
    fn repeated_sign_ins_stamp_fresh_ids_without_replacing_material() {
        for credential in [
            Credential::ApiKey(ApiKeyCredential {
                key: Some("mock-key".into()),
                ..Default::default()
            }),
            Credential::OAuth(OAuthCredential::new("mock-access", "mock-refresh", 20.0)),
        ] {
            let first = stamp_login(credential.clone());
            let second = stamp_login(first.clone());
            let mut first = serde_json::to_value(first).expect("first sign-in");
            let mut second = serde_json::to_value(second).expect("second sign-in");
            let first_id = first
                .as_object_mut()
                .expect("credential object")
                .remove("loginId");
            let second_id = second
                .as_object_mut()
                .expect("credential object")
                .remove("loginId");
            for value in [&first_id, &second_id] {
                let id = value
                    .as_ref()
                    .and_then(serde_json::Value::as_str)
                    .expect("sign-in id");
                assert_eq!(
                    uuid::Uuid::parse_str(id)
                        .expect("valid uuid")
                        .get_version_num(),
                    4
                );
            }
            assert_ne!(first_id, second_id);
            assert_eq!(
                first,
                serde_json::to_value(credential).expect("original credential")
            );
            assert_eq!(second, first);
        }
    }
}
