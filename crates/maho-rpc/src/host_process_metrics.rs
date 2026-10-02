use std::fs;
use serde::Serialize;
use crate::host_process_table::{ProcessTableRow,linux_reader};
#[derive(Debug,Serialize,PartialEq,Eq,Default)]
pub struct HostProcessMetrics { pub rss_mb:Option<i64>,pub open_fds:Option<usize>,pub zombies:Option<usize> }
pub fn descendants(rows:&[ProcessTableRow],root:u32) -> Vec<&ProcessTableRow> {
    let mut tree=rows.iter().filter(|row|row.pid==root).collect::<Vec<_>>();let mut index=0;
    while index<tree.len(){let parent=tree[index].pid;for row in rows {if row.ppid==parent&&!tree.iter().any(|item|std::ptr::eq(*item,row)){tree.push(row);}}index+=1;}tree
}
pub fn metrics_from_rows(rows:&[ProcessTableRow],pid:u32,mut descriptors:Option<&mut dyn FnMut(u32)->Option<usize>>) -> HostProcessMetrics {
    let tree=descendants(rows,pid);if tree.is_empty(){return HostProcessMetrics::default();}
    let mut open_fds=None;
    if let Some(read)=descriptors.as_mut(){for row in &tree {if let Some(count)=read(row.pid){*open_fds.get_or_insert(0)+=count;}}}
    HostProcessMetrics{rss_mb:Some((tree.iter().map(|row|row.rss_kb).sum::<f64>()/1024.+0.5).floor() as i64),open_fds,zombies:Some(tree.iter().filter(|row|row.state.starts_with('Z')).count())}
}
pub fn read_host_process_metrics(pid:u32,platform:&str) -> HostProcessMetrics {
    if platform!="linux"{return HostProcessMetrics::default();}
    let Some(rows)=linux_reader() else{return HostProcessMetrics::default();};
    metrics_from_rows(&rows,pid,Some(&mut |pid|fs::read_dir(format!("/proc/{pid}/fd")).ok().map(|entries|entries.count())))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn includes_descendants_and_counts_partial_fd_reads(){let rows=vec![ProcessTableRow{pid:1,ppid:0,state:"U".into(),rss_kb:1024.},ProcessTableRow{pid:3,ppid:2,state:"Z".into(),rss_kb:0.},ProcessTableRow{pid:2,ppid:1,state:"U".into(),rss_kb:2048.},ProcessTableRow{pid:9,ppid:0,state:"U".into(),rss_kb:9999.}];let metrics=metrics_from_rows(&rows,1,Some(&mut |pid|if pid==2{None}else{Some(4)}));assert_eq!(metrics,HostProcessMetrics{rss_mb:Some(3),open_fds:Some(8),zombies:Some(1)});assert_eq!(metrics_from_rows(&rows,8,None),HostProcessMetrics::default());}
    #[test] fn unavailable_descriptor_reader_is_not_zero(){let row=ProcessTableRow{pid:1,ppid:0,state:"U".into(),rss_kb:0.};assert_eq!(metrics_from_rows(&[row],1,Some(&mut |_|None)).open_fds,None);assert_eq!(read_host_process_metrics(1,"win32"),HostProcessMetrics::default());}
    #[test] fn real_process_surface_has_memory_and_fds(){let metrics=read_host_process_metrics(std::process::id(),"linux");assert!(metrics.rss_mb.is_some());assert!(metrics.open_fds.is_some());assert_eq!(metrics.zombies,Some(0));}
}
