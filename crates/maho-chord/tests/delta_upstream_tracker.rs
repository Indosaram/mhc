use maho_chord::{delta::*, JsonValue};
use std::result::Result;
use serde_json::json;

fn replay(prepared: &Prepared) -> Result<(), DeltaError> {
    assert_eq!(
        &apply_immutable(Some(prepared.base()), prepared.ops())?,
        prepared.value()
    );
    Ok(())
}

#[test]
fn dense_nested_regions_suppress_covered_structural_and_reserved_edits() -> Result<(), DeltaError> {
    for reserved in [false, true] {
        let rows: Vec<JsonValue> = (0..400).map(|i| {
            if i == 100 {
                if reserved { json!({"flag":0,"special":{"__proto__":{"values":(0..400).collect::<Vec<_>>()}}}).into() }
                else { json!({"flag":0,"values":(0..400).map(|v|json!({"value":v})).collect::<Vec<_>>()}).into() }
            } else { json!({"flag":0,"values":[]}).into() }
        }).collect();
        let tracker = track(JsonValue::object(vec![(
            "rows".into(),
            JsonValue::array(rows),
        )]))?;
        let mut change = tracker.begin_change();
        let rows = change.state()?.at(&["rows".into()])?;
        for index in 0..256 {
            rows.get(index)?
                .ok_or(DeltaError::Root)?
                .set("flag", &json!(1).into())?;
        }
        let row = rows.get(100usize)?.ok_or(DeltaError::Root)?;
        let values = if reserved {
            row.at(&["special".into(), "__proto__".into(), "values".into()])?
        } else {
            row.at(&["values".into()])?
        };
        for index in 0..256 {
            let value = -i64::try_from(index).map_err(|_| DeltaError::Root)? - 1;
            if reserved {
                values.set(index, &json!(value).into())?;
            } else {
                values
                    .get(index)?
                    .ok_or(DeltaError::Root)?
                    .set("value", &json!(value).into())?;
            }
        }
        if !reserved {
            values.push(&[json!({"value":999}).into()])?;
        }
        let prepared = change.prepare()?;
        assert!(matches!(prepared.ops(),[Op::Splice(path,0,256,_)] if path == &["rows".into()]));
        replay(&prepared)?;
    }
    Ok(())
}

#[test]
fn multiple_disjoint_dense_regions_keep_boundary_edits() -> Result<(), DeltaError> {
    let tracker = track(JsonValue::array(
        (0..1400)
            .map(|value| json!({"value":value}).into())
            .collect(),
    ))?;
    let mut change = tracker.begin_change();
    let array = change.state()?;
    for index in [97, 358, 897, 1158]
        .into_iter()
        .chain(100..356)
        .chain(900..1156)
    {
        array.get(index)?.ok_or(DeltaError::Root)?.set(
            "value",
            &json!(-i64::try_from(index).map_err(|_| DeltaError::Root)? - 1).into(),
        )?;
    }
    let prepared = change.prepare()?;
    assert_eq!(prepared.ops().len(), 6);
    let regions: Vec<_> = prepared
        .ops()
        .iter()
        .filter_map(|op| match op {
            Op::Splice(_, start, length, _) => Some((*start, *length)),
            _ => None,
        })
        .collect();
    assert_eq!(regions, [(100, 256), (900, 256)]);
    replay(&prepared)
}

#[test]
fn deeply_nested_dense_region_keeps_a_sparse_neighbor_narrow() -> Result<(), DeltaError> {
    let tracker = track(JsonValue::array(
        (0..2000)
            .map(|value| json!({"nested":{"value":value}}).into())
            .collect(),
    ))?;
    let mut change = tracker.begin_change();
    let array = change.state()?;
    for index in std::iter::once(0).chain(500..1000) {
        array
            .get(index)?
            .ok_or(DeltaError::Root)?
            .at(&["nested".into()])?
            .set(
                "value",
                &json!(-i64::try_from(index).map_err(|_| DeltaError::Root)? - 1).into(),
            )?;
    }
    let prepared = change.prepare()?;
    assert_eq!(prepared.ops().len(), 2);
    assert!(prepared
        .ops()
        .iter()
        .any(|op| matches!(op,Op::Splice(_,500,500,items) if items.len()==500)));
    replay(&prepared)
}

#[test]
fn structural_mutators_return_removed_values_and_match_native_result() -> Result<(), DeltaError> {
    let tracker = track(json!([3, 1, 2]).into())?;
    let mut change = tracker.begin_change();
    let array = change.state()?;
    assert_eq!(array.push(&[json!(4).into()])?, 4);
    assert_eq!(
        array.pop()?.ok_or(DeltaError::Root)?.value()?,
        json!(4).into()
    );
    assert_eq!(array.unshift(&[json!(0).into()])?, 4);
    assert_eq!(
        array.shift()?.ok_or(DeltaError::Root)?.value()?,
        json!(0).into()
    );
    let removed = array.splice(1, 1, &[json!(5).into(), json!(4).into()])?;
    assert_eq!(removed[0].value()?, json!(1).into());
    array.sort_by(|left, right| left.as_u64().cmp(&right.as_u64()))?;
    array.reverse()?;
    array.fill(&json!(9).into(), 1, Some(3))?;
    array.copy_within(1, 0, Some(2))?;
    let prepared = change.prepare()?;
    assert_eq!(prepared.value(), &json!([5, 5, 9, 2]).into());
    replay(&prepared)
}

#[test]
fn borrowed_handles_and_placements_are_independent_from_source_mutation() -> Result<(), DeltaError>
{
    let tracker = track(json!({"left":{"nested":{"value":1}},"right":null,"rows":[]}).into())?;
    let mut change = tracker.begin_change();
    let root = change.state()?;
    let left = root.at(&["left".into()])?;
    root.place("right", &left)?;
    let rows = root.at(&["rows".into()])?;
    let source = left.value()?;
    rows.push(&[source.clone(), source.clone()])?;
    rows.get(0usize)?
        .ok_or(DeltaError::Root)?
        .at(&["nested".into()])?
        .set("value", &json!(5).into())?;
    left.at(&["nested".into()])?
        .set("value", &json!(9).into())?;
    let prepared = change.prepare()?;
    assert_eq!(prepared.value(),&json!({"left":{"nested":{"value":9}},"right":{"nested":{"value":1}},"rows":[{"nested":{"value":5}},{"nested":{"value":1}}]}).into());
    assert!(!prepared.value()["rows"][0].same_identity(&prepared.value()["rows"][1]));
    replay(&prepared)
}

#[test]
fn object_keys_and_reserved_data_are_ordinary_own_fields() -> Result<(), Box<dyn std::error::Error>>
{
    let tracker =
        track(json!({"then":1,"constructor":{"value":1},"toString":2,"0":3,"a":4,"2":5}).into())?;
    let mut change = tracker.begin_change();
    let root = change.state()?;
    root.delete("a")?;
    assert!(root.get("a")?.is_none());
    root.set("a", &json!(4).into())?;
    root.at(&["constructor".into()])?
        .set("value", &json!(2).into())?;
    assert_eq!(
        root.keys()?,
        ["0", "2", "then", "constructor", "toString", "a"]
    );
    let prepared = change.prepare()?;
    assert!(matches!(prepared.ops(), [Op::Replace(_)]));
    replay(&prepared)?;
    Ok(())
}

#[test]
fn repeated_writes_sparse_null_overrides_and_length_changes_replay() -> Result<(), DeltaError> {
    let tracker = track(json!([0, 1, 2]).into())?;
    let mut change = tracker.begin_change();
    let array = change.state()?;
    array.set(1usize, &JsonValue::Null)?;
    array.set(1usize, &JsonValue::Null)?;
    assert!(array.set(5usize, &json!(9).into()).is_err());
    array.set(3usize, &json!(3).into())?;
    array.set_length(6)?;
    array.set_length(4)?;
    let prepared = change.prepare()?;
    assert_eq!(prepared.value(), &json!([0, null, 2, 3]).into());
    replay(&prepared)
}

#[test]
fn self_overlapping_fill_and_copy_within_clone_by_value() -> Result<(), DeltaError> {
    let tracker = track(json!([{ "value":1 },{ "value":2 },{ "value":3 }]).into())?;
    let mut change = tracker.begin_change();
    let array = change.state()?;
    array.copy_within(1, 0, Some(2))?;
    let source = array.get(0usize)?.ok_or(DeltaError::Root)?.value()?;
    array.fill(&source, 0, None)?;
    array
        .get(0usize)?
        .ok_or(DeltaError::Root)?
        .set("value", &json!(9).into())?;
    let prepared = change.prepare()?;
    assert_eq!(
        prepared.value(),
        &json!([{ "value":9 },{ "value":1 },{ "value":1 }]).into()
    );
    assert!(!prepared.value()[1].same_identity(&prepared.value()[2]));
    replay(&prepared)
}

#[test]
fn default_sort_uses_utf16_order_including_non_bmp_strings() -> Result<(), DeltaError> {
    let tracker = track(json!(["\u{e000}", "😀", "a", 10, 2, null]).into())?;
    let mut change = tracker.begin_change();
    change.state()?.sort()?;
    let prepared = change.prepare()?;
    assert_eq!(
        prepared.value(),
        &json!([10, 2, "a", null, "😀", "\u{e000}"]).into()
    );
    replay(&prepared)
}

#[test]
fn reverse_depth_object_edits_emit_shallow_nodes_first() -> Result<(), Box<dyn std::error::Error>> {
    let values = JsonValue::object(
        (0..150)
            .map(|i| {
                (
                    format!("field{i}").into(),
                    json!({"value":0,"child":{"value":0}}).into(),
                )
            })
            .collect(),
    );
    let tracker = track(values)?;
    let mut change = tracker.begin_change();
    let root = change.state()?;
    for i in 0..150 {
        root.at(&[Seg::Key(format!("field{i}").into()), "child".into()])?
            .set("value", &json!(2).into())?;
        root.at(&[Seg::Key(format!("field{i}").into())])?
            .set("value", &json!(1).into())?;
    }
    let prepared = change.prepare()?;
    assert_eq!(prepared.ops().len(), 300);
    assert!(prepared.ops()[..150]
        .iter()
        .all(|op| op.path().is_some_and(|p| p.len() == 2)));
    assert!(prepared.ops()[150..]
        .iter()
        .all(|op| op.path().is_some_and(|p| p.len() == 3)));
    replay(&prepared)?;
    Ok(())
}

#[test]
fn comparator_writes_and_structural_appends_are_not_lost() -> Result<(), DeltaError> {
    let tracker = track(json!([{"rank":2,"comparisons":0},{"rank":1,"comparisons":0}]).into())?;
    let mut change = tracker.begin_change();
    let array = change.state()?;
    let first = array.get(0usize)?.ok_or(DeltaError::Root)?;
    let second = array.get(1usize)?.ok_or(DeltaError::Root)?;
    let mut appended = false;
    let mut callback_error = None;
    array.sort_by(|left,right| {
        let result = (|| -> Result<(),DeltaError> {
            for item in [&first,&second] {
                let count = item.value()?["comparisons"].as_u64().ok_or(DeltaError::Root)?;
                item.set("comparisons",&json!(count+1).into())?;
            }
            if !appended {
                appended = true;
                array.set(0usize,&json!({"rank":9,"comparisons":0}).into())?;
                array.push(&[json!({"rank":3,"comparisons":0}).into()])?;
            }
            Ok(())
        })();
        if let Err(error) = result { callback_error = Some(error); }
        left["rank"].as_u64().cmp(&right["rank"].as_u64())
    })?;
    if let Some(error) = callback_error { return Err(error); }
    let prepared = change.prepare()?;
    assert_eq!(prepared.value(),&json!([{"rank":1,"comparisons":1},{"rank":2,"comparisons":1},{"rank":3,"comparisons":0}]).into());
    replay(&prepared)
}

#[test]
fn reentrant_sort_normalizes_duplicate_entries_by_value() -> Result<(), DeltaError> {
    let tracker = track(json!([{"rank":2,"nested":{"value":0}},{"rank":1,"nested":{"value":0}}]).into())?;
    let mut change = tracker.begin_change();
    let array = change.state()?;
    let first = array.get(0usize)?.ok_or(DeltaError::Root)?;
    let second = array.get(1usize)?.ok_or(DeltaError::Root)?;
    let mut inserted = false;
    let mut callback_error = None;
    array.sort_by(|left,right| {
        let result = (|| -> Result<(),DeltaError> {
            first.at(&["nested".into()])?.set("value",&json!(200).into())?;
            second.at(&["nested".into()])?.set("value",&json!(100).into())?;
            if !inserted { inserted = true; array.unshift(&[json!({"rank":4,"nested":{"value":0}}).into()])?; }
            Ok(())
        })();
        if let Err(error) = result { callback_error = Some(error); }
        left["rank"].as_u64().cmp(&right["rank"].as_u64())
    })?;
    if let Some(error) = callback_error { return Err(error); }
    let prepared = change.prepare()?;
    assert_eq!(prepared.value(),&json!([{"rank":1,"nested":{"value":100}},{"rank":2,"nested":{"value":200}},{"rank":1,"nested":{"value":100}}]).into());
    assert!(!prepared.value()[0].same_identity(&prepared.value()[2]));
    assert!(!prepared.value()[0]["nested"].same_identity(&prepared.value()[2]["nested"]));
    replay(&prepared)
}

#[tokio::test(flavor="current_thread")]
async fn transaction_survives_an_event_await_and_then_revokes() -> Result<(), Box<dyn std::error::Error>> {
    let tracker = track(json!({"then":"document-value","left":0,"nested":{"right":0}}).into())?;
    let mut change = tracker.begin_change();
    let root = change.state()?;
    root.set("left",&json!(1).into())?;
    let (send,receive) = tokio::sync::oneshot::channel();
    send.send(()).map_err(|_|"event closed")?;
    tokio::time::timeout(std::time::Duration::from_secs(1),receive).await??;
    root.at(&["nested".into()])?.set("right",&json!(2).into())?;
    let prepared = change.prepare()?;
    assert!(root.value().is_err());
    assert_eq!(prepared.value()["then"],json!("document-value").into());
    replay(&prepared)?;
    Ok(())
}

#[test]
fn raw_committed_and_draft_placements_capture_different_revisions() -> Result<(), DeltaError> {
    let tracker = track(json!({"source":{"value":1},"rawCopy":null,"draftCopy":null}).into())?;
    let raw = tracker.value()["source"].clone();
    let mut change = tracker.begin_change();
    let root = change.state()?;
    let source = root.at(&["source".into()])?;
    source.set("value",&json!(2).into())?;
    root.set("rawCopy",&raw)?;
    root.place("draftCopy",&source)?;
    let prepared = change.prepare()?;
    assert_eq!(prepared.value(),&json!({"source":{"value":2},"rawCopy":{"value":1},"draftCopy":{"value":2}}).into());
    replay(&prepared)
}

#[test]
fn ownership_is_shallow_but_all_placement_methods_copy_immediately() -> Result<(), DeltaError> {
    let input: JsonValue = json!({"property":null,"values":[{"value":0},{"value":1},{"value":2}]}).into();
    let tracker = track(input.clone())?;
    assert!(tracker.value().same_identity(&input));
    assert!(tracker.value()["values"].same_identity(&input["values"]));
    let mut change = tracker.begin_change();
    let root = change.state()?;
    let array = root.at(&["values".into()])?;
    let external: JsonValue = json!({"value":5}).into();
    root.set("property",&external)?;
    array.set(0usize,&external)?;
    array.push(std::slice::from_ref(&external))?;
    array.unshift(std::slice::from_ref(&external))?;
    array.splice(2,0,std::slice::from_ref(&external))?;
    let source = array.get(0usize)?.ok_or(DeltaError::Root)?.value()?;
    array.fill(&source,1,Some(3))?;
    array.copy_within(3,0,Some(2))?;
    let prepared = change.prepare()?;
    assert_eq!(prepared.value()["property"],json!({"value":5}).into());
    for index in 0..5 {
        assert_eq!(prepared.value()["values"][index],json!({"value":5}).into());
        assert!(!prepared.value()["values"][index].same_identity(&external));
    }
    replay(&prepared)
}

#[test]
fn deep_equal_array_and_object_assignments_detach_before_normalizing() -> Result<(), DeltaError> {
    let initial: JsonValue = json!({"object":{"nested":{"value":1}},"array":[{"value":1}],"rows":[{"value":1}]}).into();
    let tracker = track(initial.clone())?;
    let mut change = tracker.begin_change();
    let root = change.state()?;
    let old_object = root.at(&["object".into(),"nested".into()])?;
    let old_array = root.at(&["array".into(),0usize.into()])?;
    let old_row = root.at(&["rows".into(),0usize.into()])?;
    root.set("object",&json!({"nested":{"value":1}}).into())?;
    root.set("array",&json!([{"value":1}]).into())?;
    root.at(&["rows".into()])?.set(0usize,&json!({"value":1}).into())?;
    for held in [old_object,old_array,old_row] { held.set("value",&json!(9).into())?; }
    assert_eq!(root.value()?,initial);
    let prepared = change.prepare()?;
    assert!(prepared.ops().is_empty());
    assert!(prepared.value().same_identity(&initial));
    Ok(())
}

#[test]
fn normalized_assignment_preserves_original_key_order_and_readable_publications() -> Result<(), DeltaError> {
    let mut tracker = track(json!({"child":{"a":1,"b":2},"count":0}).into())?;
    let mut change = tracker.begin_change();
    let root = change.state()?;
    root.set("child",&json!({"b":2,"a":1}).into())?;
    root.set("count",&json!(1).into())?;
    let prepared = change.prepare()?;
    assert_eq!(prepared.ops(),&[Op::Set(vec!["count".into()],json!(1).into())]);
    tracker.adopt(&prepared)?;
    let publication = prepared.value().clone();
    let replacement = tracker.prepare_replace(json!({"count":2}).into())?;
    tracker.adopt(&replacement)?;
    assert_eq!(publication["count"],json!(1).into());
    assert_eq!(publication["child"].as_object().map(|v|v.iter().map(|(key,_)|key.as_str()).collect::<Vec<_>>()),Some(vec![Some("a"),Some("b")]));
    replay(&prepared)
}

#[test]
fn reverse_depth_chain_emission_matches_the_512_level_upstream_case() -> Result<(), DeltaError> {
    let mut initial: JsonValue = json!({"value":0}).into();
    for _ in 0..512 { initial = JsonValue::object(vec![("value".into(),json!(0).into()),("next".into(),initial)]); }
    let tracker = track(initial)?;
    let mut change = tracker.begin_change();
    let mut nodes = vec![change.state()?];
    while let Some(next) = nodes.last().ok_or(DeltaError::Root)?.get("next")? { nodes.push(next); }
    for (index,node) in nodes.iter().enumerate().rev() { node.set("value",&json!(index+1).into())?; }
    let prepared = change.prepare()?;
    assert_eq!(prepared.ops().len(),513);
    replay(&prepared)
}

#[test]
fn sparse_overflow_and_explicit_splice_arity_equivalents() -> Result<(), DeltaError> {
    let tracker = track(JsonValue::array((0..15_000).map(|value|json!({"value":value}).into()).collect()))?;
    let mut change = tracker.begin_change();
    let array = change.state()?;
    for index in (0..15_000).step_by(3) { array.get(index)?.ok_or(DeltaError::Root)?.set("value",&json!(-i64::try_from(index).map_err(|_|DeltaError::Root)?-1).into())?; }
    let prepared = change.prepare()?;
    assert!(matches!(prepared.ops(),[Op::Replace(_)]));
    replay(&prepared)?;
    let tracker = track(json!([1,2,3,4]).into())?;
    let mut change = tracker.begin_change();
    let array = change.state()?;
    assert!(array.splice(0,0,&[])?.is_empty());
    let removed = array.splice_to_end(1)?;
    assert_eq!(removed.iter().map(Draft::value).collect::<Result<Vec<_>,_>>()?,vec![json!(2).into(),json!(3).into(),json!(4).into()]);
    let prepared = change.prepare()?;
    assert_eq!(prepared.value(),&json!([1]).into());
    replay(&prepared)
}

#[test]
fn policy_reads_keys_serialization_and_internal_named_fields_remain_document_data() -> Result<(), Box<dyn std::error::Error>> {
    let tracker = track(json!({"values":[1,2,3],"object":{"context":1,"base":2,"parent":3,"dirty":4,"target":5,"proxy":6}}).into())?;
    let mut change = tracker.begin_change();
    let root = change.state()?;
    let values = root.at(&["values".into()])?;
    values.splice(1,1,&[json!(4).into(),json!(5).into()])?;
    assert_eq!(values.keys()?,["0","1","2","3"]);
    let doubled: Vec<u64> = (0..values.len()?).map(|i|values.get(i)?.ok_or(DeltaError::Root)?.value()?.as_u64().map(|n|n*2).ok_or(DeltaError::Root)).collect::<Result<_,_>>()?;
    assert_eq!(doubled,[2,8,10,6]);
    let object = root.at(&["object".into()])?;
    object.set("context",&json!(7).into())?;
    object.set("proxy",&json!(8).into())?;
    assert_eq!(object.keys()?,["context","base","parent","dirty","target","proxy"]);
    assert_eq!(serde_json::from_str::<JsonValue>(&serde_json::to_string(&root.value()?)?)?,root.value()?);
    assert!(values.get("map").is_err());
    let prepared = change.prepare()?;
    assert_eq!(prepared.value()["object"]["context"],json!(7).into());
    replay(&prepared)?;
    Ok(())
}

#[test]
fn integer_and_string_keys_keep_order_after_deletion_and_readdition() -> Result<(), DeltaError> {
    let tracker = track(json!({"1":"one","2":"two","first":"a","second":"b"}).into())?;
    let mut change = tracker.begin_change();
    let root = change.state()?;
    root.delete("2")?; root.set("2",&json!("two").into())?;
    root.delete("first")?; root.set("first",&json!("a").into())?;
    root.set("3",&json!("three").into())?; root.set("0",&json!("zero").into())?;
    assert_eq!(root.keys()?,["0","1","2","3","second","first"]);
    let prepared = change.prepare()?;
    assert_eq!(prepared.value().as_object().map(|v|v.iter().map(|(key,_)|key.as_str()).collect::<Vec<_>>()),Some(vec![Some("0"),Some("1"),Some("2"),Some("3"),Some("second"),Some("first")]));
    replay(&prepared)
}

#[test]
fn strict_json_boundary_rejects_nonjson_before_a_draft_can_receive_it() {
    for text in ["undefined","NaN","Infinity","[1,,2]","{\"nested\":undefined}","{\"nested\":1n}"] {
        assert!(maho_chord::json::parse(text).is_err());
    }
    assert!(serde_json::Number::from_f64(f64::NAN).is_none());
    assert!(serde_json::Number::from_f64(f64::INFINITY).is_none());
}

#[test]
fn prepared_placement_payload_is_shared_but_immutable() -> Result<(), DeltaError> {
    let tracker = track(json!([]).into())?;
    let mut change = tracker.begin_change();
    change.state()?.push(&[json!({"value":1}).into()])?;
    let prepared = change.prepare()?;
    let [Op::Splice(_,_,_,items)] = prepared.ops() else { return Err(DeltaError::Root); };
    assert!(items[0].same_identity(&prepared.value()[0]));
    let mut detached = prepared.ops().to_vec();
    if let Op::Splice(_,_,_,items) = &mut detached[0] { items.clear(); }
    assert_eq!(prepared.value(),&json!([{ "value":1 }]).into());
    replay(&prepared)
}

#[test]
fn consecutive_equal_event_arrays_detach_handles_but_normalize_before_publication() -> Result<(), DeltaError> {
    for events in [json!([]), json!([{"kind":"committed"}])] {
        let initial: JsonValue = json!({"commit":{"events":events}}).into();
        let mut tracker = track(initial.clone())?;
        for revision in 0..3 {
            let mut change = tracker.begin_change();
            let commit = change.state()?.at(&["commit".into()])?;
            let old = commit.get("events")?.ok_or(DeltaError::Root)?;
            let fresh: JsonValue = events.clone().into();
            commit.set("events", &fresh)?;
            let assigned = commit.get("events")?.ok_or(DeltaError::Root)?;
            let assigned_again = commit.get("events")?.ok_or(DeltaError::Root)?;
            assert!(!assigned.same_identity(&old));
            assert!(assigned.same_identity(&assigned_again));
            old.push(&[json!({"kind":"detached"}).into()])?;
            assert_eq!(assigned.value()?, fresh);
            let prepared = change.prepare()?;
            assert!(prepared.ops().is_empty());
            assert!(prepared.value().same_identity(&initial));
            tracker.adopt(&prepared)?;
            assert_eq!(tracker.revision(), revision + 1);
            assert!(tracker.value().same_identity(&initial));
        }
    }
    Ok(())
}

#[test]
fn changed_event_arrays_emit_once_then_equal_assignment_normalizes() -> Result<(), DeltaError> {
    let mut tracker = track(json!({"commit":{"events":[]}}).into())?;
    for events in [json!([{"kind":"committed"}]), json!([])] {
        let fresh: JsonValue = events.into();
        let mut change = tracker.begin_change();
        change.state()?.at(&["commit".into()])?.set("events", &fresh)?;
        let prepared = change.prepare()?;
        assert_eq!(prepared.ops(), &[Op::Set(vec!["commit".into(), "events".into()], fresh.clone())]);
        replay(&prepared)?;
        tracker.adopt(&prepared)?;
        let mut repeated = tracker.begin_change();
        repeated.state()?.at(&["commit".into()])?.set("events", &fresh)?;
        assert!(repeated.prepare()?.ops().is_empty());
    }
    Ok(())
}
