use aws_lc_rs::{aead, hmac};
pub fn hkdf_sha256(ikm: &[u8], salt: &[u8], info: &[u8]) -> [u8; 32] {
    let prk = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, salt), ikm);
    let tag = hmac::sign(
        &hmac::Key::new(hmac::HMAC_SHA256, prk.as_ref()),
        &[info, &[1]].concat(),
    );
    let mut key = [0; 32];
    key.copy_from_slice(tag.as_ref());
    key
}
pub fn seal(key: &[u8], nonce: [u8; 12], aad: &[u8], plain: &[u8]) -> Option<Vec<u8>> {
    let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_256_GCM, key).ok()?);
    let mut bytes = plain.to_vec();
    key.seal_in_place_append_tag(
        aead::Nonce::assume_unique_for_key(nonce),
        aead::Aad::from(aad),
        &mut bytes,
    )
    .ok()?;
    Some(bytes)
}
pub fn open(key: &[u8], nonce: [u8; 12], aad: &[u8], cipher: &[u8]) -> Option<Vec<u8>> {
    let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_256_GCM, key).ok()?);
    let mut bytes = cipher.to_vec();
    let len = key
        .open_in_place(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(aad),
            &mut bytes,
        )
        .ok()?
        .len();
    bytes.truncate(len);
    Some(bytes)
}
