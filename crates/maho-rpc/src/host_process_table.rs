use std::fs;
#[derive(Debug,Clone,PartialEq)]
pub struct ProcessTableRow { pub pid:u32,pub ppid:u32,pub state:String,pub rss_kb:f64 }
pub fn parse_linux_stat(pid:u32,stat:&str) -> Option<ProcessTableRow> {
    let tail=stat.rsplit_once(") ")?.1.split(' ').collect::<Vec<_>>();
    let state=tail.first()?.to_string();let ppid=tail.get(1)?.parse().ok()?;
    let pages=tail.get(21).and_then(|v|v.parse::<f64>().ok()).filter(|v|v.is_finite()).unwrap_or(0.);
    Some(ProcessTableRow{pid,ppid,state,rss_kb:(pages*4.+0.5).floor()})
}
pub fn linux_reader() -> Option<Vec<ProcessTableRow>> {
    let entries=fs::read_dir("/proc").ok()?;
    Some(entries.filter_map(Result::ok).filter_map(|entry|{let pid=entry.file_name().to_str()?.parse::<u32>().ok()?;if pid==0{return None;}let stat=fs::read_to_string(entry.path().join("stat")).ok()?;parse_linux_stat(pid,&stat)}).collect())
}
pub fn parse_kernel_process_table(table:&[u8],row_count:usize,self_pid:u32,self_ppid:u32,mut resident_kb:impl FnMut(u32)->f64) -> Option<Vec<ProcessTableRow>> {
    if row_count==0 || table.len()<row_count.checked_mul(648)? {return None;}
    let mut rows=Vec::new();let mut self_seen=false;
    for row in table.chunks_exact(648).take(row_count) {
        let pid=u32::from_le_bytes(row[40..44].try_into().ok()?);if pid==0{continue;}
        let ppid=u32::from_le_bytes(row[560..564].try_into().ok()?);let zombie=row[36]==5;
        if pid==self_pid {if ppid!=self_ppid||zombie{return None;}self_seen=true;}
        rows.push(ProcessTableRow{pid,ppid,state:if zombie{"Z"}else{"U"}.into(),rss_kb:resident_kb(pid)});
    }
    self_seen.then_some(rows)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn linux_parent_and_rss_after_complex_name(){let mut fields=vec!["0";22];fields[0]="Z";fields[1]="17";fields[21]="1024";let row=parse_linux_stat(42,&format!("42 (name (complex)) {}",fields.join(" "))).unwrap();assert_eq!(row.ppid,17);assert_eq!(row.rss_kb,4096.);assert_eq!(row.state,"Z");}
    #[test] fn darwin_self_check_fails_closed(){let mut table=vec![0u8;648];table[40..44].copy_from_slice(&42u32.to_le_bytes());table[560..564].copy_from_slice(&17u32.to_le_bytes());assert!(parse_kernel_process_table(&table,1,42,17,|_|100.).is_some());assert!(parse_kernel_process_table(&table,1,42,18,|_|100.).is_none());table[36]=5;assert!(parse_kernel_process_table(&table,1,42,17,|_|100.).is_none());assert!(parse_kernel_process_table(&table[..50],1,42,17,|_|100.).is_none());}
    #[test] fn actual_kernel_reader_sees_current_process(){assert!(linux_reader().unwrap().iter().any(|row|row.pid==std::process::id()));}
}
