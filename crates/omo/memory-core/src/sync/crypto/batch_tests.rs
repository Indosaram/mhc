use super::{testing::*, vendor::sync_format::object_types::*, *};
use crate::sync::keyring_port::DeviceKeyring;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
#[test]
fn golden_objects_open_to_exact_records() {
    // Given committed upstream objects, keys and expected plaintext.
    let f = fixtures();
    for object in f["json"]["objects/objects.json"]["objects"]
        .as_array()
        .expect("objects")
    {
        let file = object["file"].as_str().expect("file");
        let data = bytes(&f, &format!("objects/{file}"));
        let author = keyring(&f, object["author"].as_str().expect("author"));
        let kind = serde_json::from_value(object["kind"].clone()).expect("kind");
        let chunk = object
            .get("chunk")
            .map(|c| serde_json::from_value(c.clone()).expect("chunk"));
        let public = author.public_keys();
        let binding = BatchBinding {
            account: "encoding-account",
            epoch: object["epoch"].as_u64().expect("epoch"),
            author_id: &author.principal().id,
            signing_public_key: &public.signing,
            digest: object["sha256"].as_str().expect("digest"),
            kind,
            chunk,
        };
        // When the object crosses the authenticated receiver boundary.
        let outcome = open_batch(
            &data,
            &binding,
            &epoch_key(&f, binding.epoch).expect("key"),
            &mut create_rejection_counter(None),
        );
        // Then every framed record equals the independent upstream manifest.
        let OpenOutcome::Opened { records, .. } = outcome else {
            panic!("{file}: {outcome:?}")
        };
        let actual:Vec<_>=records.iter().map(|r|serde_json::json!({"type":r.r#type,"space_id":URL_SAFE_NO_PAD.encode(&r.space_id),"item_key":r.item_key,"body_base64url":URL_SAFE_NO_PAD.encode(&r.body)})).collect();
        assert_eq!(serde_json::json!(actual), object["payload"]);
    }
}
#[test]
fn corrupted_objects_return_no_records_and_one_reason() {
    // Given the upstream corruption matrix.
    let f = fixtures();
    let objects = f["json"]["objects/objects.json"]["objects"]
        .as_array()
        .expect("objects");
    for case in f["json"]["corrupted/corrupted.json"]["cases"]
        .as_array()
        .expect("cases")
    {
        let base = objects
            .iter()
            .find(|o| o["file"] == case["base"])
            .expect("base");
        let file = case["file"].as_str().expect("file");
        let data = bytes(&f, &format!("corrupted/{file}"));
        let author = keyring(&f, base["author"].as_str().expect("author"));
        let public = author.public_keys();
        let digest = signatures::sha256_b64(&data);
        let binding = BatchBinding {
            account: "encoding-account",
            epoch: base["epoch"].as_u64().expect("epoch"),
            author_id: &author.principal().id,
            signing_public_key: &public.signing,
            digest: &digest,
            kind: ObjectKind::Ops,
            chunk: None,
        };
        let mut counter = create_rejection_counter(None);
        // When opening a corrupted object with its actual digest.
        let result = open_batch(
            &data,
            &binding,
            &epoch_key(&f, binding.epoch).expect("key"),
            &mut counter,
        );
        // Then it exposes no records and preserves the protocol refusal.
        let OpenOutcome::Rejected { reason } = result else {
            panic!("accepted {file}")
        };
        assert_eq!(reason, case["expect"].as_str().expect("reason"), "{file}");
        assert_eq!(counter.count(), 1);
        assert_eq!(
            counter.last_event(),
            format!("sync.batch.rejected {reason}")
        );
    }
}
#[test]
fn builder_round_trips_with_fixed_inputs() {
    // Given deterministic keys and payload framing.
    let f = fixtures();
    let author = keyring(&f, "owner");
    let records = vec![SyncPayloadRecord {
        r#type: 1,
        space_id: vec![3; 16],
        item_key: "notes/a.md".into(),
        body: b"alpha".to_vec(),
    }];
    let input = BuildBatchInput {
        account: "roundtrip",
        kind: ObjectKind::Ops,
        epoch: 7,
        epoch_key: &[4; 32],
        keyring: &author,
        peers: &[],
        batch_id: &[5; 16],
        salt: &[6; 32],
        created_at: 1,
        chunk: None,
        records: &records,
    };
    // When building and opening through the real keyring and object receiver.
    let data = build_batch(&input).expect("build");
    let public = author.public_keys();
    let digest = signatures::sha256_b64(&data);
    let binding = BatchBinding {
        account: "roundtrip",
        epoch: 7,
        author_id: &author.principal().id,
        signing_public_key: &public.signing,
        digest: &digest,
        kind: ObjectKind::Ops,
        chunk: None,
    };
    let outcome = open_batch(
        &data,
        &binding,
        &[4; 32],
        &mut create_rejection_counter(None),
    );
    // Then the exact input records survive compression and authentication.
    let OpenOutcome::Opened {
        records: opened, ..
    } = outcome
    else {
        panic!("{outcome:?}")
    };
    assert_eq!(opened, records);
}
#[test]
fn wrong_digest_is_refused_before_decrypting_with_wrong_key() {
    // Given a genuine signed object bound to an incorrect chain digest and epoch key.
    let f = fixtures();
    let data = bytes(&f, "objects/batch-a-epoch1.omob");
    let author = keyring(&f, "owner");
    let public = author.public_keys();
    let binding = BatchBinding {
        account: "encoding-account",
        epoch: 1,
        author_id: &author.principal().id,
        signing_public_key: &public.signing,
        digest: "wrong",
        kind: ObjectKind::Ops,
        chunk: None,
    };
    // When opening it.
    let result = open_batch(
        &data,
        &binding,
        &[0; 32],
        &mut create_rejection_counter(None),
    );
    // Then digest binding wins over the later AEAD failure.
    assert!(matches!(
        result,
        OpenOutcome::Rejected {
            reason: "digest_mismatch"
        }
    ));
}

#[test]
fn builder_rejects_unrepresentable_record_framing() {
    let f = fixtures();
    let author = keyring(&f, "owner");
    for (space_id, item_key, problem) in [
        (vec![0; 15], "a".into(), BatchInputProblem::SpaceIdLength),
        (vec![0; 17], "a".into(), BatchInputProblem::SpaceIdLength),
        (
            vec![0; 16],
            "k".repeat(65536),
            BatchInputProblem::ItemKeyTooLong,
        ),
    ] {
        let records = [SyncPayloadRecord {
            r#type: 1,
            space_id,
            item_key,
            body: vec![1],
        }];
        let input = BuildBatchInput {
            account: "a",
            kind: ObjectKind::Ops,
            epoch: 1,
            epoch_key: &[0; 32],
            keyring: &author,
            peers: &[],
            batch_id: &[0; 16],
            salt: &[0; 32],
            created_at: 0,
            chunk: None,
            records: &records,
        };
        assert!(
            matches!(build_batch(&input),Err(BatchInputError::Framing(actual)) if actual == problem)
        );
    }
}
