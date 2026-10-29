use super::super::item::{
    frontmatter::{FmEntry, FmValue, split_item},
    normalize_item_key,
};
use std::{collections::BTreeMap, sync::LazyLock};

#[expect(
    clippy::expect_used,
    reason = "the upstream constant regular expressions are valid"
)]
static T1: LazyLock<regress::Regex> = LazyLock::new(|| {
    regress::Regex::new(r"^(.*)\.sync-conflict-(\d{8})-([A-Za-z0-9-]*)(\.[^/.]*)?$")
        .expect("T1 regex")
});
#[expect(
    clippy::expect_used,
    reason = "the upstream constant regular expressions are valid"
)]
static T2: LazyLock<regress::Regex> = LazyLock::new(|| {
    regress::Regex::new(r"(?<=^|\n)\n?<!-- memory-sync \d{4}-\d{2}-\d{2}: the other machine had different frontmatter for (.+?); kept here so no fact is lost:\n([\s\S]*?(?:\n|---))-->\n").expect("T2 regex")
});
#[expect(
    clippy::expect_used,
    reason = "the upstream constant regular expressions are valid"
)]
static T2_HEADER: LazyLock<regress::Regex> = LazyLock::new(|| {
    regress::Regex::new(r"^<!-- memory-sync \d{4}-\d{2}-\d{2}: the other machine had different frontmatter for .+; kept here so no fact is lost:$").expect("T2 header regex")
});

#[derive(Debug, PartialEq, Eq)]
pub struct SyncConflictName {
    pub target: String,
    pub day: String,
    pub label: String,
}
pub fn parse_sync_conflict_name(path: &str) -> Option<SyncConflictName> {
    let matched = T1.find(path)?;
    let stem = &path[matched.group(1)?];
    if stem.is_empty() {
        return None;
    }
    Some(SyncConflictName {
        target: format!("{stem}{}", matched.group(4).map_or("", |r| &path[r])),
        day: path[matched.group(2)?].into(),
        label: path[matched.group(3)?].into(),
    })
}
pub fn group_by_item_key(paths: &[String]) -> BTreeMap<String, Vec<String>> {
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for path in paths {
        let side = parse_sync_conflict_name(path);
        if let Some(key) =
            normalize_item_key(side.as_ref().map_or(path.as_str(), |s| s.target.as_str()))
        {
            groups.entry(key).or_default().push(path.clone());
        }
    }
    groups
}
#[derive(Debug)]
pub struct T2Strip {
    pub body: String,
    pub entries: Vec<FmEntry>,
    pub malformed: Vec<String>,
}
pub(crate) fn is_t2_header(line: &str) -> bool {
    T2_HEADER.find(line).is_some()
}
pub fn strip_t2_blocks(body: &str) -> T2Strip {
    let mut result = T2Strip {
        body: String::new(),
        entries: Vec::new(),
        malformed: Vec::new(),
    };
    let mut cursor = 0;
    for matched in T2.find_iter(body) {
        let range = matched.range();
        result.body.push_str(&body[cursor..range.start]);
        cursor = range.end;
        let kept = matched
            .group(2)
            .map_or("", |r| &body[r])
            .replace("-- >", "-->");
        let parsed = split_item(&kept);
        match parsed.fm {
            Some(entries) if parsed.body.is_empty() => result
                .entries
                .extend(entries.into_iter().filter(|e| e.value != FmValue::Remove)),
            _ => {
                result.malformed.push(body[range.clone()].into());
                result.body.push_str(&body[range]);
            }
        }
    }
    result.body.push_str(&body[cursor..]);
    result
}
