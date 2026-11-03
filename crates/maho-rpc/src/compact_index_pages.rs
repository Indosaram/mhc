//! senpi `modes/rpc/compact-index-pages.ts`: incremental descriptor packing; values and keys must
//! already be references.
use crate::compact_content_store::CompactWriteReason;
use crate::compact_read_lease::CompactError;
use serde::Serialize;

const PAGE_BYTES: usize = 65_536;
const PAGE_ROWS: usize = 128;

/// Packs rows into JSON-array pages of at most 128 rows and 64 KiB each.
pub fn pack_pages<Row: Serialize + Clone>(rows: &[Row]) -> Result<Vec<Vec<Row>>, CompactError> {
    let mut pages = Vec::new();
    let mut page: Vec<Row> = Vec::new();
    let mut bytes = 2;
    for row in rows {
        let size = serde_json::to_string(row)?.len();
        if size + 2 > PAGE_BYTES {
            return Err(CompactWriteReason::SizeLimit.into());
        }
        let separator = usize::from(!page.is_empty());
        if page.len() == PAGE_ROWS || bytes + size + separator > PAGE_BYTES {
            pages.push(std::mem::take(&mut page));
            bytes = 2;
        }
        bytes += size + usize::from(!page.is_empty());
        page.push(row.clone());
    }
    if !page.is_empty() {
        pages.push(page);
    }
    Ok(pages)
}

#[cfg(test)]
mod tests {
    use super::*;

    // senpi 4272-bounded-index-pages: pages stay within both bounds.
    #[test]
    fn packs_within_the_row_and_byte_bounds() {
        let rows: Vec<String> = (0..300).map(|n| n.to_string()).collect();
        let pages = pack_pages(&rows).expect("pack");
        assert_eq!(
            pages.iter().map(Vec::len).collect::<Vec<_>>(),
            [128, 128, 44]
        );
        let big: Vec<String> = (0..4).map(|_| "x".repeat(30_000)).collect();
        let pages = pack_pages(&big).expect("pack big");
        assert_eq!(pages.len(), 2);
        for page in &pages {
            assert!(serde_json::to_string(page).expect("json").len() <= PAGE_BYTES);
        }
        assert!(pack_pages(&["x".repeat(PAGE_BYTES)]).is_err());
    }
}
