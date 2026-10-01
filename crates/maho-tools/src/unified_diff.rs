//! jsdiff FILE_HEADERS_ONLY unified patch data; no terminal rendering.
pub fn create_unified_patch(old_path: &str, new_path: &str, old: &str, new: &str, context: usize) -> String {
    let rows = crate::edit_diff::diff_lines(old, new);
    let mut output = format!("--- {old_path}\n+++ {new_path}\n");
    let mut groups: Vec<(usize, usize)> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if row.0 == ' ' { continue; }
        let start = index.saturating_sub(context);
        let end = (index + context + 1).min(rows.len());
        if let Some(group) = groups.last_mut().filter(|g| start <= g.1) { group.1 = end; }
        else { groups.push((start,end)); }
    }
    for (start,end) in groups {
        let old_before = rows[..start].iter().filter(|r| r.0 != '+').count();
        let new_before = rows[..start].iter().filter(|r| r.0 != '-').count();
        let old_count = rows[start..end].iter().filter(|r| r.0 != '+').count();
        let new_count = rows[start..end].iter().filter(|r| r.0 != '-').count();
        let old_start = old_before + usize::from(old_count > 0);
        let new_start = new_before + usize::from(new_count > 0);
        output.push_str(&format!("@@ -{old_start},{old_count} +{new_start},{new_count} @@\n"));
        for (kind,line) in &rows[start..end] {
            output.push(*kind); output.push_str(line);
            if !line.ends_with('\n') { output.push_str("\n\\ No newline at end of file\n"); }
        }
    }
    output
}
