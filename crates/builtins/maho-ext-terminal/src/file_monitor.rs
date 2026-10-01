use std::path::{Path,PathBuf};
use crate::{terminal_manifest_model::{FileEvent,TerminalManifestCheckpoint},monitor_registry::MonitorEvent};

pub struct FileMonitor {
    pub id:String,
    pub description:String,
    path:PathBuf,
    parent:PathBuf,
    event:FileEvent,
    pub checkpoint:TerminalManifestCheckpoint,
    pub paused:bool,
    pub settled:bool,
    reservation:Option<crate::manager::CapacityReservation>,
}
#[cfg(unix)]
impl FileMonitor {
    pub fn register(id:String,description:String,path:&Path,event:FileEvent,approved_parent:Option<&Path>)->std::io::Result<Self> {
        let path=if path.is_absolute() {path.to_path_buf()} else {std::env::current_dir()?.join(path)};
        let raw_parent=path.parent().ok_or_else(||std::io::Error::other("file has no parent"))?;
        let parent=std::fs::canonicalize(raw_parent).map_err(|error|std::io::Error::other(format!("Cannot access parent directory {}: {error}",raw_parent.display())))?;
        if approved_parent.is_some_and(|approved|approved!=parent) {return Err(std::io::Error::other(format!("Cannot watch file: parent directory changed during permission approval: {}",raw_parent.display())));}
        let checkpoint=crate::durable_file::file_checkpoint(&path)?;
        Ok(Self {id,description,path,parent,event,checkpoint,paused:false,settled:false,reservation:None})
    }
    pub fn reserve_capacity(&mut self,reservation:crate::manager::CapacityReservation) {if !self.settled {self.reservation=Some(reservation);}}
    pub fn check(&mut self)->std::io::Result<Vec<MonitorEvent>> {
        if self.paused||self.settled {return Ok(vec![]);}
        let parent=std::fs::canonicalize(self.path.parent().expect("registered file parent"))?;
        if parent!=self.parent {return Err(std::io::Error::other(format!("watcher error: monitored parent changed: {}",self.path.parent().expect("registered file parent").display())));}
        if let Ok(metadata)=std::fs::symlink_metadata(&self.path) {
            use std::os::unix::fs::MetadataExt;
            if (metadata.dev() as f64!=self.checkpoint.dev||metadata.ino() as f64!=self.checkpoint.ino)&&metadata.nlink()>1 {return Err(std::io::Error::other(format!("Cannot watch file: target identity changed: {}",self.path.display())));}
        }
        let current=crate::durable_file::file_checkpoint(&self.path)?;
        let changed=match self.event {
            FileEvent::Create=>!self.checkpoint.present&&current.present,
            FileEvent::Modify=>self.checkpoint.present&&current.present&&(self.checkpoint.mtime_ms!=current.mtime_ms||self.checkpoint.size!=current.size||self.checkpoint.digest!=current.digest),
        };
        self.checkpoint=current;
        if !changed {return Ok(vec![]);}
        self.settled=true;self.reservation.take();
        Ok(vec![MonitorEvent::Line {id:self.id.clone(),description:self.description.clone(),line:format!("{} {}",match self.event {FileEvent::Create=>"create",FileEvent::Modify=>"modify"},self.path.display())},MonitorEvent::Summary {id:self.id.clone(),description:self.description.clone(),summary:"watcher completed".to_owned()}])
    }
    pub fn stop(&mut self,summary:&str)->Option<MonitorEvent> {
        if self.settled {return None;}
        self.settled=true;self.reservation.take();Some(MonitorEvent::Summary {id:self.id.clone(),description:self.description.clone(),summary:summary.to_owned()})
    }
}
#[cfg(all(test,unix))]
mod tests {
    use super::*;
    #[test]
    fn changed_identity_with_multiple_links_is_rejected()->std::io::Result<()> {
        let dir=tempfile::tempdir()?;let path=dir.path().join("watch");let replacement=dir.path().join("replacement");std::fs::write(&path,b"old")?;
        let mut monitor=FileMonitor::register("watch_1".to_owned(),"watch".to_owned(),&path,FileEvent::Modify,None)?;
        std::fs::write(&replacement,b"new")?;std::fs::remove_file(&path)?;std::fs::hard_link(&replacement,&path)?;
        assert!(monitor.check().is_err());Ok(())
    }
    #[test]
    fn create_only_fires_for_appearance_after_registration()->std::io::Result<()> {
        let dir=tempfile::tempdir()?;let path=dir.path().join("watched");let mut watch=FileMonitor::register("watch_1".to_owned(),"created".to_owned(),&path,FileEvent::Create,None)?;
        assert!(watch.check()?.is_empty());std::fs::write(&path,b"ready")?;let events=watch.check()?;
        assert_eq!(events.len(),2);assert!(matches!(&events[0],MonitorEvent::Line {line,..} if line==&format!("create {}",path.display())));assert!(watch.check()?.is_empty());assert!(watch.stop("watcher killed").is_none());
        let mut existing=FileMonitor::register("watch_2".to_owned(),"existing".to_owned(),&path,FileEvent::Create,None)?;assert!(existing.check()?.is_empty());std::fs::write(&path,b"changed")?;assert!(existing.check()?.is_empty());Ok(())
    }
    #[test]
    fn pause_keeps_baseline_and_resume_detects_change()->std::io::Result<()> {
        let dir=tempfile::tempdir()?;let path=dir.path().join("watched");std::fs::write(&path,b"before")?;
        let mut watch=FileMonitor::register("watch_1".to_owned(),"modified".to_owned(),&path,FileEvent::Modify,None)?;
        watch.paused=true;std::fs::write(&path,b"after!")?;assert!(watch.check()?.is_empty());watch.paused=false;assert_eq!(watch.check()?.len(),2);Ok(())
    }
    #[test]
    fn approval_and_regular_file_guards_fail_closed()->std::io::Result<()> {
        let dir=tempfile::tempdir()?;let path=dir.path().join("watched");
        assert!(FileMonitor::register("watch_1".to_owned(),"approval".to_owned(),&path,FileEvent::Create,Some(Path::new("/not-approved"))).is_err());
        std::os::unix::fs::symlink("missing",&path)?;assert!(FileMonitor::register("watch_1".to_owned(),"symlink".to_owned(),&path,FileEvent::Create,None).is_err());Ok(())
    }
}
