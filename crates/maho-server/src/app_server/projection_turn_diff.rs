use std::collections::BTreeMap;

#[derive(Default)]
pub struct TurnDiffTracker {
    file_diffs: BTreeMap<String, String>,
    emitted: String,
}
impl TurnDiffTracker {
    pub fn update<'a>(&mut self, tool_id: &str, diff: &str, source_order: impl IntoIterator<Item = &'a str>) -> Option<String> {
        if diff.is_empty() { return None; }
        self.file_diffs.insert(tool_id.into(), diff.into());
        let cumulative = source_order.into_iter().filter_map(|id| self.file_diffs.get(id).map(String::as_str)).collect::<String>();
        if cumulative == self.emitted { return None; }
        self.emitted = cumulative.clone();
        Some(cumulative)
    }
}
