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
pub fn parse_ps_process_table(text:&str,self_pid:u32,self_ppid:u32) -> Option<Vec<ProcessTableRow>> {
    let mut rows=Vec::new();let mut self_seen=false;
    for line in text.lines() {
        let mut fields=line.split_whitespace();
        let Some(pid)=fields.next().and_then(|v|v.parse::<u32>().ok()) else{continue;};
        let Some(ppid)=fields.next().and_then(|v|v.parse::<u32>().ok()) else{continue;};
        let Some(state)=fields.next() else{continue;};
        let Some(rss_kb)=fields.next().and_then(|v|v.parse::<f64>().ok()).filter(|v|v.is_finite()) else{continue;};
        if pid==self_pid {if ppid!=self_ppid||state.starts_with('Z'){return None;}self_seen=true;}
        rows.push(ProcessTableRow{pid,ppid,state:state.to_string(),rss_kb});
    }
    self_seen.then_some(rows)
}
#[cfg(unix)]
pub fn darwin_reader() -> Option<Vec<ProcessTableRow>> {
    use std::process::Command;
    let output=Command::new("ps").args(["-axo","pid=,ppid=,stat=,rss="]).output().ok()?;
    if !output.status.success(){return None;}
    let text=std::str::from_utf8(&output.stdout).ok()?;
    parse_ps_process_table(text,std::process::id(),std::os::unix::process::parent_id())
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
/// A process-table reader for the given platform, or nothing on a platform whose reader has
/// no native equivalent (senpi `loadProcessTableReader`).
pub type ProcessTableReader = fn() -> Option<Vec<ProcessTableRow>>;
pub fn load_process_table_reader(platform:&str) -> Option<ProcessTableReader> {
    match platform {"linux"=>Some(linux_reader),"darwin"=>darwin_reader_option(),_=>None}
}
#[cfg(unix)] fn darwin_reader_option()->Option<ProcessTableReader>{Some(darwin_reader)}
#[cfg(not(unix))] fn darwin_reader_option()->Option<ProcessTableReader>{None}
#[cfg(test)]
mod tests {
    use super::*;
    fn host_platform()->&'static str{if cfg!(target_os="linux"){"linux"}else if cfg!(target_os="macos"){"darwin"}else{panic!("unsupported host platform")}}
    #[test] fn only_linux_has_a_reader_and_it_sees_this_process(){assert!(load_process_table_reader("darwin").is_some());assert!(load_process_table_reader("win32").is_none());let linux=load_process_table_reader("linux").expect("linux reader");assert!(std::ptr::fn_addr_eq(linux,linux_reader as ProcessTableReader),"linux selects the kernel reader");let reader=load_process_table_reader(host_platform()).expect("host reader");assert!(reader().unwrap().iter().any(|row|row.pid==std::process::id()));}
    #[test] fn parses_ps_table_and_fails_closed_for_invalid_self(){let sample="10 1 Ss 2048\n20 10 S 512\nnot a process row\n30 10 Z 0\n";assert_eq!(parse_ps_process_table(sample,10,1).unwrap().len(),3);assert!(parse_ps_process_table(sample,99,1).is_none());assert!(parse_ps_process_table(sample,10,2).is_none());assert!(parse_ps_process_table(sample,30,10).is_none());}
    #[test] fn linux_parent_and_rss_after_complex_name(){let mut fields=vec!["0";22];fields[0]="Z";fields[1]="17";fields[21]="1024";let row=parse_linux_stat(42,&format!("42 (name (complex)) {}",fields.join(" "))).unwrap();assert_eq!(row.ppid,17);assert_eq!(row.rss_kb,4096.);assert_eq!(row.state,"Z");}
    #[test] fn darwin_self_check_fails_closed(){let mut table=vec![0u8;648];table[40..44].copy_from_slice(&42u32.to_le_bytes());table[560..564].copy_from_slice(&17u32.to_le_bytes());assert!(parse_kernel_process_table(&table,1,42,17,|_|100.).is_some());assert!(parse_kernel_process_table(&table,1,42,18,|_|100.).is_none());table[36]=5;assert!(parse_kernel_process_table(&table,1,42,17,|_|100.).is_none());assert!(parse_kernel_process_table(&table[..50],1,42,17,|_|100.).is_none());}
    #[test] fn actual_kernel_reader_sees_current_process(){#[cfg(target_os="linux")] let rows=linux_reader().unwrap();#[cfg(target_os="macos")] let rows=darwin_reader().unwrap();assert!(rows.iter().any(|row|row.pid==std::process::id()));}
}
