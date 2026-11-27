//! Read budget slice helper (`thread/read-budget.ts`).

use serde::Serialize;

pub fn take_within_budget<T: Serialize + Clone>(
    window: &[T],
    max_bytes: usize,
    from_end: bool,
) -> (Vec<T>, usize) {
    let mut selected = Vec::new();
    let mut encoded_bytes = 0usize;
    let iter: Box<dyn Iterator<Item = &T> + '_> = if from_end {
        Box::new(window.iter().rev())
    } else {
        Box::new(window.iter())
    };
    for entry in iter {
        let size = serde_json::to_vec(entry).map(|v| v.len()).unwrap_or(0);
        if !selected.is_empty() && encoded_bytes + size > max_bytes {
            break;
        }
        selected.push(entry.clone());
        encoded_bytes += size;
    }
    if from_end {
        selected.reverse();
    }
    (selected, encoded_bytes)
}
