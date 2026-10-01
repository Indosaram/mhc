use std::{collections::BTreeSet,time::Duration};
use tokio::process::Command;
pub async fn collect_process_tree(root:u32)->Vec<u32> {
    let mut seen=BTreeSet::new();let mut queue=vec![root];let mut index=0;
    while index<queue.len() {
        let pid=queue[index];index+=1;if !seen.insert(pid){continue;}
        let output=tokio::time::timeout(Duration::from_secs(1),Command::new("pgrep").args(["-P",&pid.to_string(),"."]).kill_on_drop(true).output()).await;
        if let Ok(Ok(output))=output && output.status.success() {
            for child in String::from_utf8_lossy(&output.stdout).split_whitespace().filter_map(|s|s.parse::<u32>().ok()).filter(|pid|*pid>0) {if !seen.contains(&child){queue.push(child);}}
        }
    }
    queue.into_iter().fold(Vec::new(),|mut result,pid|{if !result.contains(&pid){result.push(pid);}result})
}
pub async fn is_process_alive(pid:u32)->bool {
    if pid==0{return false;}
    Command::new("/usr/bin/kill").args(["-0","--",&pid.to_string()]).output().await.is_ok_and(|output|output.status.success())
}
async fn kill_pids(pids:&[u32],signal:&str) {
    for pid in pids.iter().rev().filter(|pid|**pid>1) {let _=Command::new("/usr/bin/kill").args([signal,"--",&pid.to_string()]).output().await;}
}
async fn wait_for_dead(pids:&[u32],timeout:Duration) {
    let deadline=tokio::time::Instant::now()+timeout;
    while tokio::time::Instant::now()<deadline {
        let mut alive=false;for pid in pids {if is_process_alive(*pid).await{alive=true;break;}}
        if !alive{return;}tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
pub async fn reap_process_tree(root:u32,term_wait:Duration,kill_wait:Duration) {
    let mut known=collect_process_tree(root).await;kill_pids(&known,"-TERM").await;wait_for_dead(&known,term_wait).await;
    for pid in collect_process_tree(root).await {if !known.contains(&pid){known.push(pid);}}
    kill_pids(&known,"-KILL").await;wait_for_dead(&known,kill_wait).await;
}
