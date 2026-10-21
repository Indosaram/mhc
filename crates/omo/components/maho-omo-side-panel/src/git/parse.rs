use std::collections::BTreeMap;
use crate::types::PanelGitEntry;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PanelGitDelta { pub added: u64, pub removed: u64 }

pub fn parse_porcelain_z(output: &str) -> Vec<PanelGitEntry> {
    let mut fields = output.split('\0');
    let mut entries = Vec::new();
    while let Some(field) = fields.next() {
        let (Some(xy), Some(path)) = (field.get(..2), field.get(3..)) else { continue };
        if path.is_empty() { continue; }
        let from = if xy.contains(['R', 'C']) {
            fields.next().filter(|value| !value.is_empty()).map(str::to_owned)
        } else { None };
        entries.push(PanelGitEntry { xy: xy.into(), path: path.into(), from, added: None, removed: None });
    }
    entries
}

pub fn parse_numstat_z(output: &str) -> BTreeMap<String, PanelGitDelta> {
    let mut fields = output.split('\0');
    let mut deltas = BTreeMap::new();
    while let Some(field) = fields.next() {
        let mut parts = field.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) = (parts.next(), parts.next(), parts.next()) else { continue };
        let path = if path.is_empty() { fields.next(); fields.next().unwrap_or_default() } else { path };
        if path.is_empty() { continue; }
        deltas.insert(path.to_owned(), PanelGitDelta {
            added: added.parse().unwrap_or(0), removed: removed.parse().unwrap_or(0),
        });
    }
    deltas
}

pub fn merge_git_deltas(entries: &[PanelGitEntry], unstaged: &BTreeMap<String, PanelGitDelta>, staged: &BTreeMap<String, PanelGitDelta>) -> Vec<PanelGitEntry> {
    entries.iter().map(|entry| {
        let mut entry = entry.clone();
        let a = unstaged.get(&entry.path);
        let b = staged.get(&entry.path);
        if a.is_some() || b.is_some() {
            entry.added = Some(a.map_or(0, |delta| delta.added).saturating_add(b.map_or(0, |delta| delta.added)));
            entry.removed = Some(a.map_or(0, |delta| delta.removed).saturating_add(b.map_or(0, |delta| delta.removed)));
        }
        entry
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nul_records_preserve_rename_destinations_and_unusual_names() {
        let entries = parse_porcelain_z("R  new\nname\0old name\0?? untracked\0");
        let unstaged = parse_numstat_z("2\t3\t\0old name\0new\nname\0-\t-\tbinary\0");
        let staged = parse_numstat_z("5\t1\tnew\nname\0");
        let merged = merge_git_deltas(&entries, &unstaged, &staged);
        assert_eq!(merged[0].path, "new\nname");
        assert_eq!(merged[0].from.as_deref(), Some("old name"));
        assert_eq!((merged[0].added, merged[0].removed), (Some(7), Some(4)));
        assert_eq!(merged[1].added, None);
        assert_eq!(unstaged["binary"], PanelGitDelta::default());
    }
}
