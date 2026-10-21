use super::{testing::*, vendor::sync_format::snapshot::snapshot_preimage, *};
use crate::sync::keyring_port::{DeviceKeyring, DeviceKeyringSignRequest};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
#[test]
fn snapshot_uses_header_order_and_counts_nested_failure_once() {
    let f = fixtures();
    let rows = main_chain(&f);
    let entries: Vec<_> = rows
        .iter()
        .map(|r| match envelope::parse_entry(r).expect("row") {
            SyncEntry::Keyring(e) => e,
            _ => panic!("keyring"),
        })
        .collect();
    let last = entries.last().expect("last");
    let owner = keyring(&f, "owner");
    let mut snapshot = SnapshotRecord {
        account: "encoding-account".into(),
        author: parse::parse_principal(&rows[0]["author"]).expect("author"),
        covers_seq: last.seq,
        digest: f["json"]["objects/objects.json"]["snapshot"]["digest"]
            .as_str()
            .expect("digest")
            .into(),
        link: last.fields.link.clone(),
        keyring_head: last.fields.keyring_link.clone().expect("head"),
        record_sig: String::new(),
    };
    snapshot.record_sig = URL_SAFE_NO_PAD.encode(
        owner
            .sign(DeviceKeyringSignRequest::Snapshot {
                canonical_json: &signatures::canonical(&snapshot_preimage(&snapshot))
                    .expect("canonical"),
            })
            .expect("sign"),
    );
    let mut chunks: Vec<_> = f["json"]["objects/objects.json"]["snapshot"]["chunks"]
        .as_array()
        .expect("chunks")
        .iter()
        .map(|n| bytes(&f, &format!("objects/{}", n.as_str().expect("name"))))
        .collect();
    chunks.reverse();
    let anchor = || ChainAnchor::Bootstrap(creator_anchor("encoding-account".into(), &owner));
    let mut counter = create_rejection_counter(None);
    let result = import_snapshot(
        "encoding-account",
        anchor(),
        &entries,
        &snapshot,
        &chunks,
        |e| epoch_key(&f, e),
        &mut counter,
    );
    let SnapshotOutcome::Imported { state, records } = result else {
        panic!("{result:?}")
    };
    assert_eq!(
        records
            .iter()
            .map(|r| r.item_key.as_str())
            .collect::<Vec<_>>(),
        ["snapshot part 0", "snapshot part 1"]
    );
    assert_eq!(state.head().expect("head").seq, last.seq);
    assert_eq!(counter.count(), 0);
    let result = import_snapshot(
        "encoding-account",
        anchor(),
        &entries,
        &snapshot,
        &chunks,
        |_| Some(vec![0; 32]),
        &mut counter,
    );
    assert!(matches!(
        result,
        SnapshotOutcome::Rejected {
            reason: "chunk_refused"
        }
    ));
    assert_eq!(counter.count(), 1);
    assert_eq!(counter.last_event(), "sync.batch.rejected stream_aead");
}
