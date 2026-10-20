use maho_chord::{delta::*, JsonValue};
use serde_json::{json, Value};

fn random(seed: &mut u32) -> u32 {
    *seed = seed.wrapping_add(0x6d2b79f5);
    let mut value = (*seed ^ (*seed >> 15)).wrapping_mul(1 | *seed);
    value = value.wrapping_add((value ^ (value >> 7)).wrapping_mul(61 | value)) ^ value;
    value ^ (value >> 14)
}

#[test]
fn upstream_eleven_mutation_random_transactions_match_policy_replay_and_authority(
) -> Result<(), Box<dyn std::error::Error>> {
    for seed in 1u32..=40 {
        let mut rng = seed;
        let mut expected = json!({"values":(0..4).map(|id|json!({"id":id,"score":0})).collect::<Vec<_>>(),"text":"start","meta":{"revision":0}});
        let mut tracker = track(expected.clone().into())?;
        for transaction in 0..25 {
            let base = tracker.value().clone();
            let mut change = tracker.begin_change();
            let root = change.state()?;
            for operation in 0..5 {
                let choice = (u64::from(random(&mut rng)) * 11) >> 32;
                let value = u64::from(seed) * 10_000 + transaction * 10 + operation;
                let item = json!({"id":value,"score":value%7});
                let array = root.at(&["values".into()])?;
                match choice {
                    0 => {
                        let text = format!("{}-{value}", expected["text"].as_str().ok_or("text")?);
                        expected["text"] = json!(text);
                        root.set("text", &expected["text"].clone().into())?;
                    }
                    1 => {
                        expected["values"]
                            .as_array_mut()
                            .ok_or("values")?
                            .push(item.clone());
                        array.push(&[item.into()])?;
                    }
                    2 => {
                        expected["values"]
                            .as_array_mut()
                            .ok_or("values")?
                            .insert(0, item.clone());
                        array.unshift(&[item.into()])?;
                    }
                    3 => {
                        let values = expected["values"].as_array_mut().ok_or("values")?;
                        if !values.is_empty() {
                            values.remove(0);
                            array.shift()?;
                        }
                    }
                    4 => {
                        let values = expected["values"].as_array_mut().ok_or("values")?;
                        if !values.is_empty() {
                            values.pop();
                            array.pop()?;
                        }
                    }
                    5 => {
                        let values = expected["values"].as_array_mut().ok_or("values")?;
                        let index = if values.is_empty() {
                            0
                        } else {
                            usize::try_from(value)? % (values.len() + 1)
                        };
                        let remove = if values.is_empty() {
                            0
                        } else {
                            usize::try_from(value % 2)?
                        };
                        let end = (index + remove).min(values.len());
                        values.splice(index..end, [item.clone()]);
                        array.splice(isize::try_from(index)?, remove, &[item.into()])?;
                    }
                    6 => {
                        expected["values"].as_array_mut().ok_or("values")?.reverse();
                        array.reverse()?;
                    }
                    7 => {
                        expected["values"]
                            .as_array_mut()
                            .ok_or("values")?
                            .sort_by_key(|v| v["id"].as_u64());
                        array.sort_by(|a, b| a["id"].as_u64().cmp(&b["id"].as_u64()))?;
                    }
                    8 => {
                        let values = expected["values"].as_array_mut().ok_or("values")?;
                        if !values.is_empty() {
                            let index = usize::try_from(value)? % values.len();
                            values[index]["score"] = json!(value);
                            array
                                .get(index)?
                                .ok_or("row")?
                                .set("score", &json!(value).into())?;
                        }
                    }
                    9 => {
                        let revision = expected["meta"]["revision"].as_u64().ok_or("revision")? + 1;
                        expected["meta"]["revision"] = json!(revision);
                        expected["meta"]["label"] = json!(format!("r-{value}"));
                        let meta = root.at(&["meta".into()])?;
                        meta.set("revision", &json!(revision).into())?;
                        meta.set("label", &json!(format!("r-{value}")).into())?;
                    }
                    _ => {
                        expected["meta"]
                            .as_object_mut()
                            .ok_or("meta")?
                            .remove("label");
                        root.at(&["meta".into()])?.delete("label")?;
                    }
                }
            }
            assert_eq!(root.value()?, JsonValue::from(expected.clone()));
            let prepared = change.prepare()?;
            assert_eq!(tracker.value(), &base);
            assert_eq!(prepared.value(), &JsonValue::from(expected.clone()));
            assert_eq!(
                apply_immutable(Some(&base), prepared.ops())?,
                JsonValue::from(expected.clone())
            );
            tracker.adopt(&prepared)?;
            assert!(tracker.value().same_identity(prepared.value()));
            assert_eq!(tracker.value(), &JsonValue::from(expected.clone()));
        }
    }
    Ok(())
}

#[test]
fn hundred_thousand_item_insertions_and_wide_operation_folding(
) -> Result<(), Box<dyn std::error::Error>> {
    for unshift in [false, true] {
        let tracker = track(json!([-1]).into())?;
        let mut change = tracker.begin_change();
        let array = change.state()?;
        let items: Vec<JsonValue> = (0..100_000).map(|i| json!(i).into()).collect();
        if unshift {
            array.unshift(&items)?;
        } else {
            array.splice(1, 0, &items)?;
        }
        let prepared = change.prepare()?;
        assert_eq!(prepared.value().as_array().map(Vec::len), Some(100_001));
        assert_eq!(
            prepared.value()[if unshift { 99_999 } else { 100_000 }],
            json!(99_999).into()
        );
        assert_eq!(
            &apply_immutable(Some(prepared.base()), prepared.ops())?,
            prepared.value()
        );
    }
    let tracker = track(JsonValue::object(
        (0..5000)
            .map(|i| (format!("field{i}").into(), json!(0).into()))
            .collect(),
    ))?;
    let mut change = tracker.begin_change();
    let root = change.state()?;
    for i in 0..5000 {
        root.set(Seg::Key(format!("field{i}").into()), &json!(1).into())?;
    }
    let prepared = change.prepare()?;
    assert!(matches!(prepared.ops(), [Op::Replace(_)]));
    assert_eq!(
        &apply_immutable(Some(prepared.base()), prepared.ops())?,
        prepared.value()
    );
    let cloned: Value = serde_json::from_str(&serde_json::to_string(prepared.value())?)?;
    assert_eq!(cloned["field4999"], json!(1));
    Ok(())
}
