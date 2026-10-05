use maho_server::protocol::cbor::{CborOptions, CborValue as V, decode_cbor, encode_cbor};

fn hex(s: &str) -> Vec<u8> {
    s.as_bytes()
        .chunks_exact(2)
        .map(|b| {
            u8::from_str_radix(std::str::from_utf8(b).expect("hex fixture is ASCII"), 16)
                .expect("fixture contains hex digits")
        })
        .collect()
}

#[test]
fn roundtrips_rfc8949_vectors() {
    let vectors = vec![
        (V::Null, "f6"),
        (V::Bool(false), "f4"),
        (V::Bool(true), "f5"),
        (V::Integer(0), "00"),
        (V::Integer(1), "01"),
        (V::Integer(10), "0a"),
        (V::Integer(23), "17"),
        (V::Integer(24), "1818"),
        (V::Integer(25), "1819"),
        (V::Integer(100), "1864"),
        (V::Integer(1000), "1903e8"),
        (V::Integer(1_000_000), "1a000f4240"),
        (V::Integer(1_000_000_000_000), "1b000000e8d4a51000"),
        (V::Integer(9_007_199_254_740_991), "1b001fffffffffffff"),
        (V::Integer(-1), "20"),
        (V::Integer(-10), "29"),
        (V::Integer(-24), "37"),
        (V::Integer(-25), "3818"),
        (V::Integer(-100), "3863"),
        (V::Integer(-1000), "3903e7"),
        (V::Integer(-1_000_000), "3a000f423f"),
        (V::Integer(-9_007_199_254_740_991), "3b001ffffffffffffe"),
        (V::Float(1.1), "fb3ff199999999999a"),
        (V::Float(-0.0), "fb8000000000000000"),
        (V::Bytes(vec![1, 2, 3, 4]), "4401020304"),
        (V::Text(String::new()), "60"),
        (V::Text("IETF".into()), "6449455446"),
        (V::Text("\u{fc}".into()), "62c3bc"),
        (V::Text("\u{6c34}".into()), "63e6b0b4"),
        (V::Text("\u{10151}".into()), "64f0908591"),
        (V::Array(vec![]), "80"),
        (
            V::Array(vec![V::Integer(1), V::Integer(2), V::Integer(3)]),
            "83010203",
        ),
        (
            V::Array(vec![
                V::Integer(1),
                V::Array(vec![V::Integer(2), V::Integer(3)]),
                V::Array(vec![V::Integer(4), V::Integer(5)]),
            ]),
            "8301820203820405",
        ),
        (
            V::Map(vec![
                ("a".into(), V::Integer(1)),
                ("b".into(), V::Array(vec![V::Integer(2), V::Integer(3)])),
            ]),
            "a26161016162820203",
        ),
    ];
    for (value, wire) in vectors {
        let encoded = encode_cbor(&value, CborOptions::default()).unwrap();
        assert_eq!(encoded, hex(wire), "{wire}");
        let decoded = decode_cbor(&encoded, CborOptions::default()).unwrap();
        if let V::Float(v) = value {
            let V::Float(actual) = decoded else {
                panic!("expected float")
            };
            assert_eq!(actual.to_bits(), v.to_bits());
        } else {
            assert_eq!(decoded, value);
        }
    }
}
#[test]
fn preserves_falsey_json_properties() {
    let value = serde_json::json!({"zero":0,"empty":"","no":false,"nil":null});
    let encoded = encode_cbor(&V::from_json(&value).unwrap(), CborOptions::default()).unwrap();
    assert_eq!(
        decode_cbor(&encoded, CborOptions::default())
            .unwrap()
            .into_json()
            .unwrap(),
        value
    );
}
#[test]
fn preserves_bom_and_proto_data() {
    assert_eq!(
        decode_cbor(&hex("63efbbbf"), CborOptions::default()).unwrap(),
        V::Text("\u{feff}".into())
    );
    let value = V::Map(vec![("__proto__".into(), V::Text("safe".into()))]);
    assert_eq!(
        decode_cbor(
            &encode_cbor(&value, CborOptions::default()).unwrap(),
            CborOptions::default()
        )
        .unwrap(),
        value
    );
}
#[test]
fn rejects_nonfinite_encoder_values() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(encode_cbor(&V::Float(value), CborOptions::default()).is_err());
    }
}
#[test]
fn rejects_unsafe_encoder_integers() {
    for value in [9_007_199_254_740_992, -9_007_199_254_740_992] {
        assert!(encode_cbor(&V::Integer(value), CborOptions::default()).is_err());
    }
}
#[test]
fn rejects_excessive_encoder_depth() {
    let mut value = V::Null;
    for _ in 0..65 {
        value = V::Array(vec![value]);
    }
    assert!(
        encode_cbor(&value, CborOptions::default())
            .unwrap_err()
            .to_string()
            .contains("depth")
    );
}
#[test]
fn rejects_invalid_decoder_input() {
    for wire in [
        "",
        "18",
        "1c",
        "5f",
        "7f",
        "9f",
        "bf",
        "c000",
        "f7",
        "e0",
        "ff",
        "f93c00",
        "fa3f800000",
        "fb7ff0000000000000",
        "fb7ff8000000000000",
        "fb3ff00000",
        "44010203",
        "636162",
        "8201",
        "a16161",
        "0000",
        "a10102",
        "a2616101616102",
        "61ff",
        "62c080",
        "63eda080",
        "1b0020000000000000",
        "3b001fffffffffffff",
        "fb4340000000000000",
    ] {
        assert!(
            decode_cbor(&hex(wire), CborOptions::default()).is_err(),
            "{wire}"
        );
    }
}
#[test]
fn rejects_excessive_decoder_depth() {
    let mut bytes = vec![0x81; 65];
    bytes.push(0xf6);
    assert!(
        decode_cbor(&bytes, CborOptions::default())
            .unwrap_err()
            .to_string()
            .contains("depth")
    );
}
#[test]
fn rejects_declared_lengths_before_traversing() {
    for wire in ["5a01000001", "7a01000001", "9a000f4241", "ba000f4241"] {
        assert!(
            decode_cbor(&hex(wire), CborOptions::default())
                .unwrap_err()
                .to_string()
                .contains("limit")
        );
    }
}
#[test]
fn honors_caller_container_limits() {
    let options = CborOptions {
        max_container_length: 2,
        ..CborOptions::default()
    };
    assert!(decode_cbor(&hex("83010203"), options).is_err());
    assert!(
        encode_cbor(
            &V::Array(vec![V::Integer(1), V::Integer(2), V::Integer(3)]),
            options
        )
        .is_err()
    );
}
#[test]
fn honors_caller_byte_limits() {
    let options = CborOptions {
        max_byte_length: 2,
        ..CborOptions::default()
    };
    assert!(decode_cbor(&hex("626162"), options).is_err());
    assert!(encode_cbor(&V::Text("ab".into()), options).is_err());
}
#[test]
fn rejects_invalid_configured_depth() {
    assert!(
        CborOptions {
            max_depth: 513,
            ..CborOptions::default()
        }
        .resolve()
        .is_err()
    );
}

#[test]
fn integral_float_uses_integer_wire_representation() {
    for (value, wire) in [(1.0, "01"), (-1.0, "20"), (1_000_000.0, "1a000f4240")] {
        assert_eq!(
            encode_cbor(&V::Float(value), CborOptions::default()).unwrap(),
            hex(wire)
        );
    }
}
