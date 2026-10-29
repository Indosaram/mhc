use super::{testing::*, *};
use std::collections::BTreeMap;

#[test]
fn raw_transport_wraps_cannot_cross_the_verified_capability_boundary() {
    let raw = WrapEntry {
        epoch: 1,
        id: "device:unknown".into(),
        wrap: "AA".into(),
    };
    assert!(matches!(
        open_unverified_wrap(&raw),
        WrapOutcome::Rejected {
            reason: "unverified_wrap"
        }
    ));
}
#[test]
fn golden_history_requires_exact_origins_and_binding() {
    let f = fixtures();
    for wrap in f["json"]["wraps.json"]["wraps"].as_array().expect("wraps") {
        let expected: BTreeMap<_, _> = wrap["history_plaintext"]
            .as_array()
            .expect("history")
            .iter()
            .map(|r| {
                (
                    r["epoch"].as_u64().expect("epoch"),
                    r["origin"].as_str().expect("origin").into(),
                )
            })
            .collect();
        let epoch = wrap["epoch"].as_u64().expect("epoch");
        let id = wrap["id"].as_str().expect("id");
        let material = &wrap["key_material"];
        let opened = open_history(material, "encoding-account", epoch, id, &expected)
            .expect("golden history opens");
        assert_eq!(opened.1.len(), expected.len());
        assert!(open_history(material, "foreign", epoch, id, &expected).is_none());
        let mut unknown = expected.clone();
        unknown.insert(999, "unknown".into());
        assert!(open_history(material, "encoding-account", epoch, id, &unknown).is_none());
        if let Some(first) = expected.keys().next() {
            let mut missing = expected.clone();
            missing.remove(first);
            assert!(open_history(material, "encoding-account", epoch, id, &missing).is_none());
        }
    }
}
