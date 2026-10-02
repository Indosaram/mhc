use crate::types::HistoryEntry;
pub fn filter_history(entries:&[HistoryEntry],query:&str)->Vec<HistoryEntry>{
    let query=query.trim();if query.is_empty(){return entries.to_vec();}
    let newest=entries.iter().map(|entry|entry.timestamp).max().unwrap_or(0).max(0);
    let mut scored:Vec<(&HistoryEntry,f64)>=entries.iter().filter_map(|entry|{
        let result=maho_tui::fuzzy::fuzzy_match(query,&entry.text);
        result.matches.then(||(entry,result.score+(newest.saturating_sub(entry.timestamp).max(0).to_string().parse::<f64>().unwrap_or(0.0)/86_400_000.0)*0.01))
    }).collect();
    scored.sort_by(|(left,ls),(right,rs)|ls.total_cmp(rs).then_with(||right.timestamp.cmp(&left.timestamp)));
    scored.into_iter().map(|(entry,_)|entry.clone()).collect()
}
