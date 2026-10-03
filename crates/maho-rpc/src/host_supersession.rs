use crate::socket_ownership::EndpointOwnership;
/// Handle for the supersession watcher task; dropping or stopping it ends the watch.
pub struct SupersessionWatch{task:tokio::task::JoinHandle<()>}
impl SupersessionWatch{pub fn stop(self){self.task.abort();}}
/// Runs `on_lost` once this endpoint stops being served by the entry this generation bound
/// (senpi `watchForSupersession`). Without an identity, supersession can never be proven and is
/// never claimed, so the watch is inert.
pub fn watch_for_supersession(path:String,identity:Option<crate::socket_ownership::SocketFileIdentity>,settled:impl Fn()->bool+Send+Sync+'static,on_lost:impl FnOnce(EndpointLoss)+Send+'static)->SupersessionWatch{
    let task=tokio::spawn(async move{if let Some(loss)=wait_for_supersession(&path,identity,settled).await{on_lost(loss);}});
    SupersessionWatch{task}
}
pub const SUPERSESSION_POLL_MS:u64=1000;
pub const ABSENT_CONFIRMATIONS:u32=3;
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub enum EndpointLoss{Replaced,Absent}
#[derive(Default)]
pub struct SupersessionWatcher{absent_polls:u32,lost:bool}
pub async fn wait_for_supersession(path:&str,identity:Option<crate::socket_ownership::SocketFileIdentity>,settled:impl Fn()->bool)->Option<EndpointLoss>{
    use std::os::unix::fs::MetadataExt;
    let identity=identity?;
    let period=std::time::Duration::from_millis(SUPERSESSION_POLL_MS);
    let mut timer=tokio::time::interval_at(tokio::time::Instant::now()+period,period);
    timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut watcher=SupersessionWatcher::default();
    loop{
        timer.tick().await;
        if settled(){continue;}
        let ownership=match tokio::fs::metadata(path).await{
            Ok(stat) if stat.dev()==identity.dev&&stat.ino()==identity.ino=>EndpointOwnership::Held,
            Ok(_)=>EndpointOwnership::Replaced,
            Err(error) if error.kind()==std::io::ErrorKind::NotFound=>EndpointOwnership::Absent,
            Err(_)=>EndpointOwnership::Unknown,
        };
        if let Some(loss)=watcher.observe(ownership,settled()){return Some(loss);}
    }
}
impl SupersessionWatcher{
    pub fn observe(&mut self,ownership:EndpointOwnership,settled:bool)->Option<EndpointLoss>{
        if settled||self.lost{return None;}
        let loss=match ownership{
            EndpointOwnership::Replaced=>Some(EndpointLoss::Replaced),
            EndpointOwnership::Absent=>{self.absent_polls+=1;(self.absent_polls>=ABSENT_CONFIRMATIONS).then_some(EndpointLoss::Absent)},
            EndpointOwnership::Held|EndpointOwnership::Unknown=>{self.absent_polls=0;None}
        };
        if loss.is_some(){self.lost=true;}loss
    }
}
#[cfg(test)]
mod tests{
    use super::*;
    #[tokio::test]async fn watch_reports_a_replaced_endpoint_once(){let temp=tempfile::tempdir().unwrap();let path=temp.path().join("socket");let original=std::os::unix::net::UnixListener::bind(&path).unwrap();let identity=crate::socket_ownership::stat_socket_identity(&path).unwrap();let replacement=temp.path().join("new");let newer=std::os::unix::net::UnixListener::bind(&replacement).unwrap();std::fs::rename(replacement,&path).unwrap();let(lost_tx,lost_rx)=tokio::sync::oneshot::channel();let watch=watch_for_supersession(path.to_string_lossy().into_owned(),identity,||false,move|loss|{let _=lost_tx.send(loss);});let loss=tokio::time::timeout(std::time::Duration::from_secs(2),lost_rx).await.unwrap().unwrap();assert_eq!(loss,EndpointLoss::Replaced);watch.stop();drop((original,newer));}
    #[test] fn unknown_resets_consecutive_absence_and_loss_latches(){let mut watcher=SupersessionWatcher::default();assert_eq!(watcher.observe(EndpointOwnership::Absent,false),None);watcher.observe(EndpointOwnership::Unknown,false);for _ in 0..2{assert_eq!(watcher.observe(EndpointOwnership::Absent,false),None);}assert_eq!(watcher.observe(EndpointOwnership::Absent,false),Some(EndpointLoss::Absent));assert_eq!(watcher.observe(EndpointOwnership::Replaced,false),None);}
    #[test] fn settled_observation_is_ignored(){let mut watcher=SupersessionWatcher::default();assert_eq!(watcher.observe(EndpointOwnership::Replaced,true),None);assert_eq!(watcher.observe(EndpointOwnership::Replaced,false),Some(EndpointLoss::Replaced));}
}
