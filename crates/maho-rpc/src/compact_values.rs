//! senpi `modes/rpc/compact-values.ts`: field/array descriptors and string chunks live in bounded
//! leaves; owners share immutable subtrees.
//!
//! Rust input values are `serde_json::Value`: they cannot form a cycle or hold a non-finite number or
//! a class instance, so `cycle` and `unsupported_value` are never produced from a `Value` (the reasons
//! stay in `CompactWriteReason` for parity).
use crate::compact_content_store::{
    CompactContentStore, CompactWriteReason, LeafId, utf16_len, utf16_slice,
};
use crate::compact_read_lease::{CompactError, CompactReadReason};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

const MAX_VALUE_BYTES: i64 = 16 * 1024 * 1024;
const PAGE_ITEMS: usize = 128;
const STRING_CHUNK_UNITS: usize = 4096;

/// A stored value: `node`/`owner` leaves, or an inline scalar (`node == 0`, `owner == 0`, `value`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValueRef {
    pub node: LeafId,
    pub owner: LeafId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
}

impl ValueRef {
    pub fn inline(value: Value) -> Self {
        Self {
            node: 0,
            owner: 0,
            value: Some(value),
        }
    }

    pub fn is_inline(&self) -> bool {
        self.node == 0
    }
}

pub type Field = (ValueRef, ValueRef);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ValuePage {
    Scalar {
        value: Value,
    },
    String {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next: Option<ValueRef>,
    },
    Array {
        items: Vec<ValueRef>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next: Option<ValueRef>,
    },
    Object {
        items: Vec<Field>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next: Option<ValueRef>,
    },
}

impl ValuePage {
    fn next(&self) -> Option<&ValueRef> {
        match self {
            Self::Scalar { .. } => None,
            Self::String { next, .. } | Self::Array { next, .. } | Self::Object { next, .. } => {
                next.as_ref()
            }
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Scalar { .. } => "scalar",
            Self::String { .. } => "string",
            Self::Array { .. } => "array",
            Self::Object { .. } => "object",
        }
    }
}

/// One resident decoded page; dropping it ends its residency.
pub struct HeldPage {
    pub value: ValuePage,
    counter: Arc<AtomicI64>,
}

impl Drop for HeldPage {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::SeqCst);
    }
}

fn ownership() -> CompactError {
    CompactError::read(CompactReadReason::Ownership)
}

#[derive(Clone)]
pub struct CompactValues {
    store: CompactContentStore,
    resident_pages: Arc<AtomicI64>,
    peak_resident_pages: Arc<AtomicI64>,
}

impl CompactValues {
    pub fn new(store: CompactContentStore) -> Self {
        Self {
            store,
            resident_pages: Arc::new(AtomicI64::new(0)),
            peak_resident_pages: Arc::new(AtomicI64::new(0)),
        }
    }

    pub fn resident_pages(&self) -> i64 {
        self.resident_pages.load(Ordering::SeqCst)
    }

    pub fn peak_resident_pages(&self) -> i64 {
        self.peak_resident_pages.load(Ordering::SeqCst)
    }

    pub fn record(&self, fields: &[(String, ValueRef)]) -> Result<ValueRef, CompactError> {
        let mut next: Option<ValueRef> = None;
        let mut end = fields.len();
        loop {
            let start = end.saturating_sub(PAGE_ITEMS);
            let mut items = Vec::new();
            for (key, value) in &fields[start..end] {
                items.push((self.own(&Value::String(key.clone()))?, value.clone()));
            }
            let reference = self.make(&ValuePage::Object {
                items: items.clone(),
                next: next.clone(),
            })?;
            for (key, _) in &items {
                self.store.adjust(key.owner, -1, 0)?;
            }
            if let Some(next) = &next {
                self.store.adjust(next.owner, -1, 0)?;
            }
            next = Some(reference);
            if end <= PAGE_ITEMS {
                break;
            }
            end = start;
        }
        next.ok_or_else(ownership)
    }

    pub fn own(&self, value: &Value) -> Result<ValueRef, CompactError> {
        let mut owned: Vec<LeafId> = Vec::new();
        let mut walk = Walk {
            values: self,
            nodes: 0,
            bytes: 0,
            owned: &mut owned,
        };
        match walk.visit(value, 0) {
            Ok(reference) => Ok(reference),
            Err(error) => {
                for owner in owned {
                    let _ = self.store.adjust(owner, -1, 0);
                }
                Err(error)
            }
        }
    }

    pub async fn page(&self, reference: &ValueRef) -> Result<HeldPage, CompactError> {
        let resident = self.resident_pages.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_resident_pages
            .fetch_max(resident, Ordering::SeqCst);
        let held = |value| HeldPage {
            value,
            counter: self.resident_pages.clone(),
        };
        let decoded = async {
            let text = self.store.read_leaf(reference.node).await?;
            if text.len() > 65_536 {
                return Err(CompactError::read(CompactReadReason::SizeLimit));
            }
            let page: ValuePage = serde_json::from_str(&text)?;
            let count = match &page {
                ValuePage::Object { items, .. } => items.len(),
                ValuePage::Array { items, .. } => items.len(),
                _ => 0,
            };
            if count > PAGE_ITEMS {
                return Err(CompactError::read(CompactReadReason::CountLimit));
            }
            Ok(page)
        }
        .await;
        match decoded {
            Ok(page) => Ok(held(page)),
            Err(error) => {
                self.resident_pages.fetch_sub(1, Ordering::SeqCst);
                Err(error)
            }
        }
    }

    pub fn read<'a>(
        &'a self,
        reference: &'a ValueRef,
    ) -> BoxFuture<'a, Result<Value, CompactError>> {
        Box::pin(async move {
            if reference.is_inline() {
                return Ok(reference.value.clone().unwrap_or(Value::Null));
            }
            let mut cursor = Some(reference.clone());
            let mut text = String::new();
            let mut array = Vec::new();
            let mut object = Map::new();
            let mut kind: Option<&'static str> = None;
            while let Some(current) = cursor {
                let held = self.page(&current).await?;
                let page = &held.value;
                if *kind.get_or_insert(page.kind()) != page.kind() {
                    return Err(ownership());
                }
                match page {
                    ValuePage::Scalar { value } => return Ok(value.clone()),
                    ValuePage::String { text: chunk, .. } => text.push_str(chunk),
                    ValuePage::Array { items, .. } => {
                        for child in items {
                            array.push(self.read(child).await?);
                        }
                    }
                    ValuePage::Object { items, .. } => {
                        for (key, value) in items {
                            let Value::String(name) = self.read(key).await? else {
                                return Err(ownership());
                            };
                            object.insert(name, self.read(value).await?);
                        }
                    }
                }
                cursor = page.next().cloned();
            }
            Ok(match kind {
                Some("string") => Value::String(text),
                Some("array") => Value::Array(array),
                _ => Value::Object(object),
            })
        })
    }

    /// The object's fields in order: `(name, value, key)`.
    ///
    /// PERF-DIVERGENCE(senpi async generator): upstream yields field by field; this returns the
    /// descriptors of every page (refs only, never payloads).
    pub async fn fields(
        &self,
        reference: &ValueRef,
    ) -> Result<Vec<(String, ValueRef, ValueRef)>, CompactError> {
        let mut result = Vec::new();
        let mut cursor = Some(reference.clone());
        while let Some(current) = cursor {
            let held = self.page(&current).await?;
            let ValuePage::Object { items, next } = &held.value else {
                return Err(ownership());
            };
            for (key, value) in items {
                let Value::String(name) = self.read(key).await? else {
                    return Err(ownership());
                };
                result.push((name, value.clone(), key.clone()));
            }
            cursor = next.clone();
        }
        Ok(result)
    }

    /// Patches are coalesced before this call; copy descriptors, never the fields' payloads.
    pub async fn merge(
        &self,
        base: &ValueRef,
        patch: &[(String, ValueRef)],
    ) -> Result<ValueRef, CompactError> {
        let mut reversed: Option<ValueRef> = None;
        let mut result: Option<ValueRef> = None;
        let outcome = self
            .merge_into(base, patch, &mut reversed, &mut result)
            .await;
        if outcome.is_err() {
            if let Some(reversed) = &reversed {
                let _ = self.store.adjust(reversed.owner, -1, 0);
            }
            if let Some(result) = &result {
                let _ = self.store.adjust(result.owner, -1, 0);
            }
        }
        outcome
    }

    async fn flush_page(
        &self,
        buffer: &mut Vec<Field>,
        reversed: &mut Option<ValueRef>,
    ) -> Result<(), CompactError> {
        let reference = self.make(&ValuePage::Object {
            items: std::mem::take(buffer),
            next: reversed.clone(),
        })?;
        if let Some(previous) = reversed.replace(reference.clone()) {
            self.store.adjust(previous.owner, -1, 0)?;
        }
        self.store.flush_leaf(reference.node).await?;
        if !self.store.is_committed(reference.node) {
            return Err(CompactWriteReason::StorageUnavailable.into());
        }
        Ok(())
    }

    async fn merge_into(
        &self,
        base: &ValueRef,
        patch: &[(String, ValueRef)],
        reversed: &mut Option<ValueRef>,
        result: &mut Option<ValueRef>,
    ) -> Result<ValueRef, CompactError> {
        let mut buffer: Vec<Field> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (name, value, key) in self.fields(base).await? {
            let patched = patch.iter().find(|(field, _)| *field == name);
            if patched.is_some() {
                seen.insert(name.clone());
            }
            buffer.push((key, patched.map_or(value, |(_, value)| value.clone())));
            if buffer.len() == PAGE_ITEMS {
                self.flush_page(&mut buffer, reversed).await?;
            }
        }
        for (name, value) in patch {
            if seen.contains(name) {
                continue;
            }
            let key = self.own(&Value::String(name.clone()))?;
            buffer.push((key.clone(), value.clone()));
            // The key's initial reference is transferred to the completed page below.
            let flushed = self.flush_page(&mut buffer, reversed).await;
            self.store.adjust(key.owner, -1, 0)?;
            flushed?;
        }
        if !buffer.is_empty() || reversed.is_none() {
            self.flush_page(&mut buffer, reversed).await?;
        }
        while let Some(current) = reversed.clone() {
            let held = self.page(&current).await?;
            let ValuePage::Object { items, next } = &held.value else {
                return Err(ownership());
            };
            let reference = self.make(&ValuePage::Object {
                items: items.clone(),
                next: result.clone(),
            })?;
            if let Some(previous) = result.replace(reference.clone()) {
                self.store.adjust(previous.owner, -1, 0)?;
            }
            self.store.flush_leaf(reference.node).await?;
            if !self.store.is_committed(reference.node) {
                return Err(CompactWriteReason::StorageUnavailable.into());
            }
            if let Some(next) = next {
                self.store.adjust(next.owner, 1, 0)?;
            }
            self.store.adjust(current.owner, -1, 0)?;
            *reversed = next.clone();
        }
        result.take().ok_or_else(ownership)
    }

    fn make(&self, page: &ValuePage) -> Result<ValueRef, CompactError> {
        let mut children: Vec<LeafId> = match page {
            ValuePage::Object { items, .. } => items
                .iter()
                .flat_map(|(key, value)| [key.owner, value.owner])
                .collect(),
            ValuePage::Array { items, .. } => items.iter().map(|item| item.owner).collect(),
            _ => Vec::new(),
        };
        if let Some(next) = page.next() {
            children.push(next.owner);
        }
        let text = serde_json::to_string(page)?;
        self.store.with(|state| {
            let node = state.create_leaf(&text, true)?;
            let mut refs = vec![node];
            refs.extend(children);
            let owner = state.text_own(&refs)?;
            state.adjust(node, -1, 0)?;
            Ok(ValueRef {
                node,
                owner,
                value: None,
            })
        })
    }
}

struct Walk<'a> {
    values: &'a CompactValues,
    nodes: usize,
    bytes: i64,
    owned: &'a mut Vec<LeafId>,
}

impl Walk<'_> {
    fn take(&mut self, page: ValuePage, children: &[ValueRef]) -> Result<ValueRef, CompactError> {
        self.bytes += i64::try_from(serde_json::to_string(&page)?.len()).unwrap_or(i64::MAX);
        if self.bytes > MAX_VALUE_BYTES {
            return Err(CompactWriteReason::SizeLimit.into());
        }
        let reference = self.values.make(&page)?;
        self.owned.push(reference.owner);
        for child in children {
            self.values.store.adjust(child.owner, -1, 0)?;
            if let Some(index) = self.owned.iter().position(|owner| *owner == child.owner) {
                self.owned.swap_remove(index);
            }
        }
        Ok(reference)
    }

    fn visit(&mut self, value: &Value, depth: usize) -> Result<ValueRef, CompactError> {
        if depth > 128 {
            return Err(CompactWriteReason::DepthLimit.into());
        }
        self.nodes += 1;
        if self.nodes > 100_000 {
            return Err(CompactWriteReason::CountLimit.into());
        }
        if let Value::String(text) = value
            && utf16_len(text) * 2 > MAX_VALUE_BYTES
        {
            return Err(CompactWriteReason::SizeLimit.into());
        }
        let scalar = !matches!(value, Value::String(_) | Value::Array(_) | Value::Object(_));
        if scalar || (value.is_string() && utf16_len(&value.to_string()) <= 64) {
            self.bytes += i64::try_from(value.to_string().len()).unwrap_or(i64::MAX);
            if self.bytes > MAX_VALUE_BYTES {
                return Err(CompactWriteReason::SizeLimit.into());
            }
            return Ok(ValueRef::inline(value.clone()));
        }
        let mut next: Option<ValueRef> = None;
        match value {
            Value::String(text) => {
                let mut end = usize::try_from(utf16_len(text)).unwrap_or(0);
                while end > 0 {
                    let start = end.saturating_sub(STRING_CHUNK_UNITS);
                    let children: Vec<ValueRef> = next.iter().cloned().collect();
                    next = Some(self.take(
                        ValuePage::String {
                            text: utf16_slice(text, start, end).to_owned(),
                            next: next.clone(),
                        },
                        &children,
                    )?);
                    end = start;
                }
            }
            Value::Array(items) => {
                if self.nodes + items.len() > 100_000 {
                    return Err(CompactWriteReason::CountLimit.into());
                }
                let mut end = items.len();
                loop {
                    let start = end.saturating_sub(PAGE_ITEMS);
                    let mut refs = Vec::new();
                    for item in &items[start..end] {
                        refs.push(self.visit(item, depth + 1)?);
                    }
                    let mut children = refs.clone();
                    children.extend(next.iter().cloned());
                    next = Some(self.take(
                        ValuePage::Array {
                            items: refs,
                            next: next.clone(),
                        },
                        &children,
                    )?);
                    if end <= PAGE_ITEMS {
                        break;
                    }
                    end = start;
                }
            }
            Value::Object(object) => {
                if self.nodes + object.len() > 100_000 {
                    return Err(CompactWriteReason::CountLimit.into());
                }
                let entries: Vec<(&String, &Value)> = object.iter().collect();
                let mut end = entries.len();
                loop {
                    let start = end.saturating_sub(PAGE_ITEMS);
                    let mut fields = Vec::new();
                    for (key, item) in &entries[start..end] {
                        let key = self.visit(&Value::String((*key).clone()), depth + 1)?;
                        fields.push((key, self.visit(item, depth + 1)?));
                    }
                    let mut children: Vec<ValueRef> = fields
                        .iter()
                        .flat_map(|(key, value)| [key.clone(), value.clone()])
                        .collect();
                    children.extend(next.iter().cloned());
                    next = Some(self.take(
                        ValuePage::Object {
                            items: fields,
                            next: next.clone(),
                        },
                        &children,
                    )?);
                    if end <= PAGE_ITEMS {
                        break;
                    }
                    end = start;
                }
            }
            _ => {}
        }
        next.ok_or_else(|| CompactWriteReason::UnsupportedValue.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_content_store::CompactContentStoreOptions;
    use serde_json::json;

    fn store(root: &tempfile::TempDir) -> CompactContentStore {
        CompactContentStore::new(CompactContentStoreOptions {
            dir: root.path().join("recovery/epoch-1").display().to_string(),
            ..Default::default()
        })
    }

    // senpi 4272-structured-value-codec: owned input with bounded pages, released to zero.
    #[tokio::test]
    async fn owns_nested_input_and_oversized_keys_in_bounded_pages() {
        let root = tempfile::tempdir().expect("tempdir");
        let store = store(&root);
        let codec = CompactValues::new(store.clone());
        let key = "\u{AC00}".repeat(40_000);
        let body = format!("\u{FFFD}{}", "x".repeat(100_000));
        let mut input = Map::new();
        input.insert(key.clone(), Value::String(body.clone()));
        input.insert("nested".into(), json!({"n":1}));
        let input = Value::Object(input);
        let reference = codec.own(&input).expect("own");
        store.idle().await;
        let mut entries = tokio::fs::read_dir(store.dir()).await.expect("dir");
        while let Some(entry) = entries.next_entry().await.expect("entry") {
            if entry.file_name().to_string_lossy().starts_with("value-") {
                assert!(entry.metadata().await.expect("meta").len() <= 65_536);
            }
        }
        assert_eq!(codec.read(&reference).await.expect("read"), input);
        assert_eq!(store.status().structured_bytes.resident, 0);
        store.adjust(reference.owner, -1, 0).expect("release");
        store.idle().await;
        assert_eq!(store.status().structured_bytes, Default::default());
        assert_eq!(store.text().resident_nodes(), 0);
        store.retire().await.expect("retire");
    }

    #[tokio::test]
    async fn traverses_count_bounded_array_pages_without_truncating() {
        for count in [128usize, 129] {
            let root = tempfile::tempdir().expect("tempdir");
            let store = store(&root);
            let codec = CompactValues::new(store.clone());
            let input = Value::Array((0..count).map(|n| json!(n)).collect());
            let reference = codec.own(&input).expect("own");
            store.idle().await;
            let mut cursor = Some(reference.clone());
            let mut seen = 0;
            while let Some(current) = cursor {
                let page = codec.page(&current).await.expect("page");
                let ValuePage::Array { items, next } = &page.value else {
                    panic!("expected array descriptors");
                };
                assert!(items.len() <= 128);
                seen += items.len();
                cursor = next.clone();
            }
            assert_eq!(seen, count);
            assert_eq!(codec.read(&reference).await.expect("read"), input);
            store.adjust(reference.owner, -1, 0).expect("release");
            store.idle().await;
            assert_eq!(codec.resident_pages(), 0);
            store.retire().await.expect("retire");
        }
    }

    #[tokio::test]
    async fn merge_copies_descriptors_and_appends_new_fields() {
        let root = tempfile::tempdir().expect("tempdir");
        let store = store(&root);
        let codec = CompactValues::new(store.clone());
        let base = codec.own(&json!({"a":1,"b":"two"})).expect("base");
        let patch = vec![
            ("b".to_owned(), codec.own(&json!(2)).expect("b")),
            ("c".to_owned(), codec.own(&json!([3])).expect("c")),
        ];
        let merged = codec.merge(&base, &patch).await.expect("merge");
        assert_eq!(
            codec.read(&merged).await.expect("read"),
            json!({"a":1,"b":2,"c":[3]})
        );
        for owner in [base.owner, merged.owner, patch[0].1.owner, patch[1].1.owner] {
            store.adjust(owner, -1, 0).expect("release");
        }
        store.idle().await;
        assert_eq!(store.status().structured_bytes.spilled, 0);
        assert_eq!(store.status().structured_bytes.resident, 0);
        store.retire().await.expect("retire");
    }
}
