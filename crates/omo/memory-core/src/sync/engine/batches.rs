use super::journal::WireItem;

/// sync-design §6: at most 8 MiB of plaintext per batch, a hard bound.
pub const MAX_BATCH_BYTES: usize = 8 * 1024 * 1024;

fn plaintext_bytes(item: &WireItem) -> usize {
    item.ops.len() * 3 / 4
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OversizedItemError(pub String);

impl std::fmt::Display for OversizedItemError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for OversizedItemError {}

/// Splits pending items between items, in capture order, so no batch exceeds `limit` plaintext
/// bytes. An item that alone exceeds the bound is refused here; the item store must slice it by
/// version range before it reaches the push path.
pub fn split_batches(
    items: &[WireItem],
    limit: usize,
) -> Result<Vec<Vec<WireItem>>, OversizedItemError> {
    let mut batches = Vec::new();
    let mut current: Vec<WireItem> = Vec::new();
    let mut size = 0usize;
    for item in items {
        let bytes = plaintext_bytes(item);
        if bytes > limit {
            return Err(OversizedItemError(format!(
                "item {} ({bytes} bytes) exceeds the {limit}-byte batch bound",
                item.key
            )));
        }
        if !current.is_empty() && size + bytes > limit {
            batches.push(std::mem::take(&mut current));
            size = 0;
        }
        current.push(item.clone());
        size += bytes;
    }
    if !current.is_empty() {
        batches.push(current);
    }
    Ok(batches)
}

#[cfg(test)]
#[path = "batches_tests.rs"]
mod tests;
