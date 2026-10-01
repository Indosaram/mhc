use super::types::{ReadBraceScan, ReadFoldRange, ReadFolderResult, ReadLineRange};

pub fn hierarchy(ranges: &[ReadLineRange]) -> Option<Vec<ReadFoldRange>> {
    let mut sorted = ranges.to_vec();
    sorted.sort_by_key(|r| (r.start_line, std::cmp::Reverse(r.end_line)));
    fn insert(nodes: &mut Vec<ReadFoldRange>, range: ReadLineRange) -> bool {
        if let Some(parent) = nodes.last_mut()
            && range.start_line <= parent.end_line
        {
            if range.start_line == parent.start_line && range.end_line == parent.end_line {
                return true;
            }
            if range.start_line <= parent.start_line || range.end_line >= parent.end_line {
                return false;
            }
            return insert(&mut parent.children, range);
        }
        nodes.push(ReadFoldRange {
            start_line: range.start_line,
            end_line: range.end_line,
            children: Vec::new(),
        });
        true
    }
    let mut roots = Vec::new();
    for range in sorted {
        if !insert(&mut roots, range) {
            return None;
        }
    }
    Some(roots)
}
pub fn compose_fold_result(text: &str, scan: ReadBraceScan) -> ReadFolderResult {
    match scan {
        ReadBraceScan::ParseFailure { reason } => ReadFolderResult::ParseFailure { reason },
        ReadBraceScan::Parsed { ranges } => match hierarchy(&ranges) {
            Some(ranges) => ReadFolderResult::Parsed {
                text: text.into(),
                ranges,
            },
            None => ReadFolderResult::ParseFailure {
                reason: "ambiguous_line_boundaries".into(),
            },
        },
    }
}
