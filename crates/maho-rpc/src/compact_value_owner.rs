//! senpi `modes/rpc/compact-value-owner.ts`: owns one JSON-normal value as chained bounded pages
//! without recursion or an aggregate size, depth or node cap (S03b-4, S10-Q2).

use crate::compact_content_store::{
    CompactContentStore, CompactWriteReason, LeafId, utf16_len, utf16_slice,
};
use crate::compact_read_lease::CompactError;
use crate::compact_values::{ValuePage, ValueRef};
use serde_json::Value;

const VALUE_PAGE_ITEMS: usize = 128;
const VALUE_PAGE_CODE_UNITS: usize = 4096;
const INLINE_JSON_LENGTH: usize = 64;

enum FrameEntries<'a> {
    Array(&'a [Value]),
    Object(Vec<(&'a String, &'a Value)>),
}

impl<'a> FrameEntries<'a> {
    #[allow(dead_code)]
    fn len(&self) -> usize {
        match self {
            Self::Array(items) => items.len(),
            Self::Object(pairs) => pairs.len(),
        }
    }
}

struct Frame<'a> {
    array: bool,
    entries: FrameEntries<'a>,
    end: usize,
    children: Vec<&'a Value>,
    key_strings: Vec<Option<String>>,
    index: usize,
    refs: Vec<ValueRef>,
    next: Option<ValueRef>,
}

fn scalar_inline(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => true,
        Value::String(s) => {
            serde_json::to_string(s).is_ok_and(|json| json.len() <= INLINE_JSON_LENGTH)
        }
        _ => false,
    }
}

/// Owns one JSON-normal value as chained bounded pages without recursion or an aggregate cap.
pub fn own_value(
    store: &CompactContentStore,
    make: &mut dyn FnMut(&ValuePage) -> Result<ValueRef, CompactError>,
    value: &Value,
) -> Result<ValueRef, CompactError> {
    let mut owned: Vec<LeafId> = Vec::new();
    let mut stack: Vec<Frame<'_>> = Vec::new();

    let take = |owned: &mut Vec<LeafId>,
                make: &mut dyn FnMut(&ValuePage) -> Result<ValueRef, CompactError>,
                page: &ValuePage,
                children: &[&ValueRef]|
     -> Result<ValueRef, CompactError> {
        let r = make(page)?;
        owned.push(r.owner);
        for child in children {
            store.adjust(child.owner, -1, 0)?;
            if let Some(pos) = owned.iter().position(|&id| id == child.owner) {
                owned.swap_remove(pos);
            }
        }
        Ok(r)
    };

    let own_string = |owned: &mut Vec<LeafId>,
                      make: &mut dyn FnMut(&ValuePage) -> Result<ValueRef, CompactError>,
                      text: &str|
     -> Result<ValueRef, CompactError> {
        let total = usize::try_from(utf16_len(text)).unwrap_or(0);
        let mut next: Option<ValueRef> = None;
        let mut end = total;
        while end > 0 {
            let start = end.saturating_sub(VALUE_PAGE_CODE_UNITS);
            let chunk = utf16_slice(text, start, end).to_owned();
            let page = ValuePage::String {
                text: chunk,
                next: next.clone(),
            };
            let child_slice: Vec<&ValueRef> = next.iter().collect();
            next = Some(take(owned, make, &page, &child_slice)?);
            end = start;
        }
        next.ok_or_else(|| CompactWriteReason::UnsupportedValue.into())
    };

    let slice_frame = |frame: &mut Frame<'_>| {
        let start = frame.end.saturating_sub(VALUE_PAGE_ITEMS);
        let end = frame.end;
        frame.children.clear();
        frame.key_strings.clear();
        match &frame.entries {
            FrameEntries::Array(items) => {
                for item in &items[start..end] {
                    frame.children.push(item);
                    frame.key_strings.push(None);
                }
            }
            FrameEntries::Object(pairs) => {
                for (k, v) in &pairs[start..end] {
                    frame.key_strings.push(Some((*k).clone()));
                    frame.children.push(v);
                }
            }
        }
        frame.index = 0;
        frame.refs.clear();
    };

    let res = (|| -> Result<ValueRef, CompactError> {
        let mut pending: Option<&Value> = Some(value);
        let mut pending_key: Option<String> = None;
        let mut done: Option<ValueRef> = None;

        loop {
            if let Some(key) = pending_key.take() {
                let r = if key.len() + 2 <= INLINE_JSON_LENGTH
                    && serde_json::to_string(&key).is_ok_and(|j| j.len() <= INLINE_JSON_LENGTH)
                {
                    ValueRef::inline(Value::String(key))
                } else {
                    own_string(&mut owned, make, &key)?
                };
                if let Some(frame) = stack.last_mut() {
                    frame.refs.push(r);
                }
            }
            if let Some(current) = pending.take() {
                if scalar_inline(current) {
                    done = Some(ValueRef::inline(current.clone()));
                } else if let Value::String(text) = current {
                    done = Some(own_string(&mut owned, make, text)?);
                } else if let Value::Array(items) = current {
                    let mut frame = Frame {
                        array: true,
                        entries: FrameEntries::Array(items),
                        end: items.len(),
                        children: Vec::new(),
                        key_strings: Vec::new(),
                        index: 0,
                        refs: Vec::new(),
                        next: None,
                    };
                    slice_frame(&mut frame);
                    stack.push(frame);
                } else if let Value::Object(map) = current {
                    let entries: Vec<(&String, &Value)> = map.iter().collect();
                    let len = entries.len();
                    let mut frame = Frame {
                        array: false,
                        entries: FrameEntries::Object(entries),
                        end: len,
                        children: Vec::new(),
                        key_strings: Vec::new(),
                        index: 0,
                        refs: Vec::new(),
                        next: None,
                    };
                    slice_frame(&mut frame);
                    stack.push(frame);
                } else {
                    return Err(CompactWriteReason::UnsupportedValue.into());
                }
            }

            if let Some(ref_val) = done.take() {
                let Some(frame) = stack.last_mut() else {
                    return Ok(ref_val);
                };
                frame.refs.push(ref_val);
            }

            let Some(frame) = stack.last_mut() else {
                return Err(CompactWriteReason::UnsupportedValue.into());
            };
            if frame.index < frame.children.len() {
                let idx = frame.index;
                frame.index += 1;
                pending_key = frame.key_strings[idx].take();
                pending = Some(frame.children[idx]);
            } else {
                let refs = std::mem::take(&mut frame.refs);
                let prev_next = frame.next.take();
                let page = if frame.array {
                    ValuePage::Array {
                        items: refs.clone(),
                        next: prev_next.clone(),
                    }
                } else {
                    let mut fields = Vec::with_capacity(refs.len() / 2);
                    for pair in refs.chunks_exact(2) {
                        fields.push((pair[0].clone(), pair[1].clone()));
                    }
                    ValuePage::Object {
                        items: fields,
                        next: prev_next.clone(),
                    }
                };
                let mut child_refs: Vec<&ValueRef> = refs.iter().collect();
                if let Some(ref n) = prev_next {
                    child_refs.push(n);
                }
                let next_ref = take(&mut owned, make, &page, &child_refs)?;
                frame.next = Some(next_ref);
                if frame.end > VALUE_PAGE_ITEMS {
                    frame.end -= VALUE_PAGE_ITEMS;
                    slice_frame(frame);
                } else {
                    let popped = stack.pop().ok_or(CompactWriteReason::UnsupportedValue)?;
                    done = popped.next;
                }
            }
        }
    })();

    if res.is_err() {
        for owner in owned {
            let _ = store.adjust(owner, -1, 0);
        }
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_content_store::CompactContentStoreOptions;
    use crate::compact_values::CompactValues;
    use serde_json::json;

    #[tokio::test]
    async fn owns_large_strings_arrays_and_objects_iteratively_and_reads_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let values = CompactValues::new(store.clone());

        let long_str = "z".repeat(5000);
        let big_arr: Vec<Value> = (0..140).map(|i| json!(i)).collect();
        let input = json!({"long": long_str, "arr": big_arr, "short": "ok"});

        let mut make = |page: &ValuePage| -> Result<ValueRef, CompactError> {
            let text = serde_json::to_string(page)?;
            let leaf = store.create_leaf(&text, true)?;
            let mut refs = vec![leaf];
            match page {
                ValuePage::String { next: Some(n), .. } => refs.push(n.owner),
                ValuePage::Array { items, next } => {
                    for it in items {
                        refs.push(it.owner);
                    }
                    if let Some(n) = next {
                        refs.push(n.owner);
                    }
                }
                ValuePage::Object { items, next } => {
                    for (k, v) in items {
                        refs.push(k.owner);
                        refs.push(v.owner);
                    }
                    if let Some(n) = next {
                        refs.push(n.owner);
                    }
                }
                _ => {}
            }
            let owner = store.text().own(&refs)?;
            store.adjust(leaf, -1, 0)?;
            Ok(ValueRef {
                node: leaf,
                owner,
                value: None,
            })
        };

        let root = own_value(&store, &mut make, &input).expect("own");
        let decoded = values.read(&root).await.expect("read back");
        assert_eq!(decoded, input);

        store.adjust(root.owner, -1, 0).expect("release");
        store.idle().await;
        assert_eq!(store.status().structured_bytes.resident, 0);
        store.retire().await.expect("retire");
    }

    #[tokio::test]
    async fn failure_in_make_releases_all_created_owners() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let mut count = 0usize;
        let mut make = |page: &ValuePage| -> Result<ValueRef, CompactError> {
            count += 1;
            if count == 2 {
                return Err(CompactWriteReason::SizeLimit.into());
            }
            let leaf = store.create_leaf(&serde_json::to_string(page)?, true)?;
            Ok(ValueRef {
                node: leaf,
                owner: leaf,
                value: None,
            })
        };
        let input = json!({"long": "x".repeat(5000), "b": "y".repeat(5000)});
        assert!(own_value(&store, &mut make, &input).is_err());
        store.idle().await;
        assert_eq!(store.status().structured_bytes.resident, 0);
        store.retire().await.expect("retire");
    }
}
