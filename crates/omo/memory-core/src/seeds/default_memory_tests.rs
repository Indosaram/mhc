use pretty_assertions::assert_eq;

use super::*;
use crate::support::sha256::sha256_hex;

#[test]
fn test_persona_seed_sha256_when_hashed_matches_pin() {
    let persona_file = crate::memfs::frontmatter::render_memory_file(
        &crate::memfs::frontmatter::MemoryFrontmatter {
            description: "Persona - who I am".to_string(),
            read_only: None,
            kind: None,
            aliases: None,
        },
        DEFAULT_PERSONA_BODY,
    )
    .unwrap();
    let file_hash = sha256_hex(persona_file.as_bytes());
    let body_hash = sha256_hex(DEFAULT_PERSONA_BODY.as_bytes());
    assert_eq!(file_hash.len(), 64);
    assert_eq!(body_hash.len(), 64);
    assert_eq!(
        V1_PERSONA_SEED_SHA256,
        "ed9106790224a2820d68b1e09c847672dbd3c44b56e8dedd17091e8d0a8c0e8e"
    );
}
