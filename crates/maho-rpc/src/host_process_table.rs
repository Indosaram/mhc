#[cfg(any(target_os="linux",target_os="macos"))]
use std::fs;
#[derive(Debug,Clone,PartialEq)]
pub struct ProcessTableRow { pub pid:u32,pub ppid:u32,pub state:String,pub rss_kb:f64 }
#[cfg(target_os="linux")]
pub fn parse_linux_stat(pid:u32,stat:&str) -> Option<ProcessTableRow> {
    let tail=stat.rsplit_once(") ")?.1.split(' ').collect::<Vec<_>>();
    let state=tail.first()?.to_string();let ppid=tail.get(1)?.parse().ok()?;
    let pages=tail.get(21).and_then(|v|v.parse::<f64>().ok()).filter(|v|v.is_finite()).unwrap_or(0.);
    Some(ProcessTableRow{pid,ppid,state,rss_kb:(pages*4.+0.5).floor()})
}
#[cfg(target_os="linux")]
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
/// A process-table reader for the given platform, or nothing on a platform whose reader has
/// no native equivalent (senpi `loadProcessTableReader`).
pub type ProcessTableReader = fn() -> Option<Vec<ProcessTableRow>>;
pub fn load_process_table_reader(platform:&str) -> Option<ProcessTableReader> {
    match platform {
        #[cfg(target_os="linux")]
        "linux" => Some(linux_reader as ProcessTableReader),
        #[cfg(target_os="macos")]
        "darwin"|"macos" => Some(darwin_reader as ProcessTableReader),
    #[cfg(not(unix))]
    "linux"|"darwin"|"macos" => None,
        _ => None,
    }
}
/// `p_stat` SZOMB: the kernel's zombie state; every live state is a different value.
#[cfg(target_os="macos")]
const SZOMB:u32=5;
/// The darwin table through libproc, never a `ps` spawn (senpi `darwinReader`). senpi reads
/// `sysctl(KERN_PROC_ALL)` through `bun:ffi`; this crate forbids `unsafe`, so the same kernel table
/// is read through libproc's safe `proc_listpids` + `proc_pidinfo` (arg 1 = zombies included).
/// Rows the kernel refuses (other users' processes) are absent, which leaves the daemon's own tree
/// intact. Fails closed like `parseKernelProcessTable`: no self row, a self ppid that disagrees
/// with the kernel's view, or a zombie self answers `None`.
#[cfg(target_os="macos")]
pub fn darwin_reader() -> Option<Vec<ProcessTableRow>> {
    use libproc::{bsd_info::BSDInfo,task_info::TaskInfo,proc_pid::pidinfo,processes::{ProcFilter,pids_by_type}};
    let own=std::process::id();let mut self_seen=false;let mut rows=Vec::new();
    for pid in pids_by_type(ProcFilter::All).ok()? {
        let Ok(raw)=i32::try_from(pid) else{continue;};if raw<=0{continue;}
        let Ok(info)=pidinfo::<BSDInfo>(raw,1) else{continue;};
        let zombie=info.pbi_status==SZOMB;
        if pid==own {if info.pbi_ppid!=std::os::unix::process::parent_id()||zombie{return None;}self_seen=true;}
        let rss_kb=if zombie{0.}else{pidinfo::<TaskInfo>(raw,0).map_or(0.,|task|(task.pti_resident_size as f64/1024.+0.5).floor())};
        rows.push(ProcessTableRow{pid,ppid:info.pbi_ppid,state:if zombie{"Z"}else{"U"}.into(),rss_kb});
    }
    self_seen.then_some(rows)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    fn kernel_platform()->&'static str{if cfg!(target_os="macos"){"darwin"}else{"linux"}}
    #[cfg(unix)]
    #[test] fn only_kernel_platforms_have_a_reader_and_it_sees_this_process(){assert!(load_process_table_reader("win32").is_none());assert!(load_process_table_reader("freebsd").is_none());#[cfg(target_os="linux")]assert!(std::ptr::fn_addr_eq(load_process_table_reader("linux").expect("linux reader"),linux_reader as ProcessTableReader),"linux selects the /proc reader");#[cfg(target_os="macos")]{assert!(std::ptr::fn_addr_eq(load_process_table_reader("darwin").expect("darwin reader"),darwin_reader as ProcessTableReader),"darwin selects the libproc reader");}#[cfg(target_os="linux")]{assert!(load_process_table_reader("darwin").is_none());}let reader=load_process_table_reader(kernel_platform()).expect("own platform reader");assert!(reader().unwrap().iter().any(|row|row.pid==std::process::id()));}
    #[cfg(target_os="linux")]
    #[test] fn linux_parent_and_rss_after_complex_name(){let mut fields=vec!["0";22];fields[0]="Z";fields[1]="17";fields[21]="1024";let row=parse_linux_stat(42,&format!("42 (name (complex)) {}",fields.join(" "))).unwrap();assert_eq!(row.ppid,17);assert_eq!(row.rss_kb,4096.);assert_eq!(row.state,"Z");}
    #[cfg(target_os="macos")]
    #[test] fn darwin_self_check_fails_closed(){let mut table=vec![0u8;648];table[40..44].copy_from_slice(&42u32.to_le_bytes());table[560..564].copy_from_slice(&17u32.to_le_bytes());assert!(parse_kernel_process_table(&table,1,42,17,|_|100.).is_some());assert!(parse_kernel_process_table(&table,1,42,18,|_|100.).is_none());table[36]=5;assert!(parse_kernel_process_table(&table,1,42,17,|_|100.).is_none());assert!(parse_kernel_process_table(&table[..50],1,42,17,|_|100.).is_none());}
    #[cfg(unix)]
    #[test] fn actual_kernel_reader_sees_current_process(){let rows=load_process_table_reader(kernel_platform()).expect("reader")().expect("table");let own=rows.iter().find(|row|row.pid==std::process::id()).expect("own row");assert_eq!(own.ppid,std::os::unix::process::parent_id());assert!(own.rss_kb>0.);assert!(!own.state.starts_with('Z'));}
    #[cfg(unix)]
    #[cfg(target_os="linux")]
    #[test] fn actual_kernel_reader_reports_an_unreaped_child_as_zombie(){let mut child=std::process::Command::new("/usr/bin/true").spawn().unwrap();rustix::process::waitid(rustix::process::WaitId::Pid(rustix::process::Pid::from_child(&child)),rustix::process::WaitIdOptions::EXITED|rustix::process::WaitIdOptions::NOWAIT).unwrap();let rows=load_process_table_reader(kernel_platform()).expect("reader")().expect("table");let row=rows.iter().find(|row|row.pid==child.id()).expect("zombie row");assert!(row.state.starts_with('Z'));assert_eq!(row.ppid,std::process::id());assert_eq!(row.rss_kb,0.);child.wait().unwrap();}
}
