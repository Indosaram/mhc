use pretty_assertions::assert_eq;

use super::*;

#[test]
fn test_sha256_when_input_is_empty_then_it_matches_the_fips_vector() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn test_sha256_when_input_is_abc_then_it_matches_the_fips_vector() {
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn test_sha256_when_input_spans_several_blocks_then_it_matches_the_fips_vector() {
    assert_eq!(
        sha256_hex(b"The quick brown fox jumps over the lazy dog"),
        "d7a8fbb307d7809469ca9abcb0082e4f8d5651e46d3cdb762d02d0bf37c9e592"
    );
}

#[test]
fn test_hex_when_bytes_are_given_then_it_is_lowercase_and_zero_padded() {
    assert_eq!(hex(&[0x00, 0x0f, 0xff]), "000fff");
    assert_eq!(hex(&[]), "");
}
