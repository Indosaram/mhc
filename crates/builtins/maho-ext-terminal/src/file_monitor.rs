use std::path::{Path,PathBuf};
use crate::{terminal_manifest_model::{FileEvent,TerminalManifestCheckpoint},monitor_registry::MonitorEvent};

/// Symlink hop budget for `realpath_without_open` (upstream `MAX_SYMLINK_HOPS`).
const MAX_SYMLINK_HOPS:usize=40;

/// Resolves symlinks without open(2)-ing a component (upstream `resolveWithoutOpen` /
/// `realpathWithoutOpen`). Components are processed SEQUENTIALLY from one queue - `.`, `..` and
/// normal parts alike - so `/tmp/a/../b` resolves to `/tmp/b`, never `/tmp/a/b`, and a relative
/// symlink target is spliced in at the front of the queue before its own `..` is applied. This uses
/// `symlink_metadata`/`read_link` instead of `std::fs::canonicalize`, which opens every component it
/// resolves (an execute-only directory fails with EACCES, and resolving can block on an autofs
/// trigger). Not opening is not the same as never mounting; the walker only avoids the open-based
/// resolution path. A component that cannot be lstat'd (or a hop overflow) is kept verbatim from
/// that point on, so a not-yet-created tail still lands where its symlinked parent points.
#[cfg(unix)]
pub fn realpath_without_open(input:&Path)->PathBuf {
    use std::path::Component;
    fn join(base:&Path,parts:impl Iterator<Item=std::ffi::OsString>)->PathBuf {let mut path=base.to_path_buf();for part in parts {path.push(part);}path}
    let absolute=if input.is_absolute() {input.to_path_buf()} else {std::env::current_dir().unwrap_or_default().join(input)};
    let mut root=PathBuf::new();
    let mut pending:std::collections::VecDeque<std::ffi::OsString>=std::collections::VecDeque::new();
    for component in absolute.components() {
        match component {
            Component::RootDir|Component::Prefix(_)=>{root.push(component.as_os_str());},
            Component::CurDir=>pending.push_back(".".into()),
            Component::ParentDir=>pending.push_back("..".into()),
            Component::Normal(part)=>pending.push_back(part.to_os_string()),
        }
    }
    let mut resolved:Vec<std::ffi::OsString>=Vec::new();
    let mut hops=0usize;
    while let Some(part)=pending.pop_front() {
        if part=="." {continue;}
        if part==".." {resolved.pop();continue;}
        let candidate=join(&root,resolved.iter().cloned()).join(&part);
        match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.file_type().is_symlink()=>{
                hops+=1;
                if hops>MAX_SYMLINK_HOPS {return join(&candidate,pending.into_iter());}
                match std::fs::read_link(&candidate) {
                    Ok(link)=>{
                        if link.is_absolute() {resolved.clear();root=link.components().find_map(|component|matches!(component,Component::RootDir|Component::Prefix(_)).then(||component.as_os_str().to_owned())).map(PathBuf::from).unwrap_or_default();}
                        let link_parts=link.components().filter_map(|component|match component {Component::Normal(part)=>Some(part.to_os_string()),Component::CurDir=>Some(".".into()),Component::ParentDir=>Some("..".into()),_=>None}).collect::<Vec<_>>();
                        for item in link_parts.into_iter().rev() {pending.push_front(item);}
                    },
                    Err(_)=>return join(&candidate,pending.into_iter()),
                }
            },
            _=>resolved.push(part),
        }
    }
    join(&root,resolved.into_iter())
}

pub struct FileMonitor {
    pub id:String,
    pub description:String,
    path:PathBuf,
    parent:PathBuf,
    event:FileEvent,
    identity:(f64,f64),
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
        // Identity is derived without open(2), with the same walker the permission parser used, so
        // the two sides agree byte-for-byte and neither can block on a wedged mount.
        let parent=realpath_without_open(raw_parent);
        if approved_parent.is_some_and(|approved|approved!=parent) {return Err(std::io::Error::other(format!("Cannot watch file: parent directory changed during permission approval: {}",raw_parent.display())));}
        // Target identity: the resolved target must be exactly the approved parent plus the watched
        // basename, so a symlink or a swap during approval is refused instead of silently watched.
        let resolved_target=realpath_without_open(&path);
        let expected_parent=approved_parent.unwrap_or(&parent);
        if resolved_target!=expected_parent.join(path.file_name().unwrap_or_default()) {return Err(std::io::Error::other(format!("Cannot watch file: target identity changed: {}",path.display())));}
        let checkpoint=crate::durable_file::file_checkpoint(&path)?;
        Ok(Self {id,description,path,parent,event,identity:(checkpoint.dev,checkpoint.ino),checkpoint,paused:false,settled:false,reservation:None})
    }
    pub fn reserve_capacity(&mut self,reservation:crate::manager::CapacityReservation) {if !self.settled {self.reservation=Some(reservation);}}
    pub fn check(&mut self)->std::io::Result<Vec<MonitorEvent>> {
        if self.paused||self.settled {return Ok(vec![]);}
        let parent=realpath_without_open(self.path.parent().expect("registered file parent"));
        if parent!=self.parent {return Err(std::io::Error::other(format!("watcher error: monitored parent changed: {}",self.path.parent().expect("registered file parent").display())));}
        let current=crate::durable_file::file_checkpoint_with_identity(&self.path,Some(self.identity))?;
        let changed=match self.event {
            FileEvent::Create=>!self.checkpoint.present&&current.present,
            FileEvent::Modify=>self.checkpoint.present&&current.present&&(self.checkpoint.mtime_ms!=current.mtime_ms||self.checkpoint.size!=current.size||self.checkpoint.digest!=current.digest),
        };
        if !self.checkpoint.present&&current.present {self.identity=(current.dev,current.ino);}
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
    fn parent_symlink_retargeting_is_rejected_before_reading_target()->std::io::Result<()> {
        let dir=tempfile::tempdir()?;let first=dir.path().join("first");let second=dir.path().join("second");std::fs::create_dir(&first)?;std::fs::create_dir(&second)?;std::fs::write(first.join("file"),b"first")?;std::fs::write(second.join("file"),b"second")?;
        let parent=dir.path().join("parent");std::os::unix::fs::symlink(&first,&parent)?;let mut monitor=FileMonitor::register("watch_1".to_owned(),"watch".to_owned(),&parent.join("file"),FileEvent::Modify,Some(&first))?;let saved=monitor.checkpoint.clone();
        std::fs::remove_file(&parent)?;std::os::unix::fs::symlink(&second,&parent)?;assert!(monitor.check().unwrap_err().to_string().contains("monitored parent changed"));assert_eq!(monitor.checkpoint,saved);assert!(!monitor.settled);Ok(())
    }
    #[test]
    fn create_watch_keeps_original_identity_until_absent_to_present()->std::io::Result<()> {
        let dir=tempfile::tempdir()?;let path=dir.path().join("watch");let replacement=dir.path().join("replacement");std::fs::write(&path,b"old")?;
        let mut monitor=FileMonitor::register("watch_1".to_owned(),"watch".to_owned(),&path,FileEvent::Create,None)?;let identity=monitor.identity;
        std::fs::write(&replacement,b"new")?;std::fs::rename(&replacement,&path)?;assert!(monitor.check()?.is_empty());assert_eq!(monitor.identity,identity);assert_ne!(monitor.checkpoint.ino,identity.1);
        std::fs::hard_link(&path,&replacement)?;assert!(monitor.check().is_err());std::fs::remove_file(&replacement)?;std::fs::remove_file(&path)?;assert!(monitor.check()?.is_empty());
        std::fs::write(&path,b"created")?;assert_eq!(monitor.check()?.len(),2);assert_eq!(monitor.identity,(monitor.checkpoint.dev,monitor.checkpoint.ino));Ok(())
    }
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
    #[test]
    fn realpath_without_open_resolves_symlinked_parents_and_keeps_missing_tails_verbatim() {
        let dir=tempfile::tempdir().unwrap();let real=dir.path().join("real");std::fs::create_dir(&real).unwrap();let link=dir.path().join("link");std::os::unix::fs::symlink(&real,&link).unwrap();
        assert_eq!(realpath_without_open(&link),std::fs::canonicalize(&real).unwrap());
        let missing=link.join("not-created-yet");
        assert_eq!(realpath_without_open(&missing),std::fs::canonicalize(&real).unwrap().join("not-created-yet"));
    }
    #[test]
    fn dot_dot_is_applied_after_the_queued_parts_that_precede_it() {
        let dir=tempfile::tempdir().unwrap();let a=dir.path().join("a");std::fs::create_dir(&a).unwrap();
        let expected=dir.path().join("b");
        assert_eq!(realpath_without_open(&a.join("..").join("b")),expected,"a/../b must resolve to b, not a/b");
        // `..` pops the already-resolved prefix, so `a/../../a` escapes the temp dir into its parent
        // (upstream `resolveWithoutOpen` applies each `..` to the resolved stack, never lexically).
        assert_eq!(realpath_without_open(&a.join("..").join("..").join(a.file_name().unwrap())),dir.path().parent().unwrap().join("a"));
    }
    #[test]
    fn a_relative_symlink_target_is_spliced_before_its_own_dot_dot() {
        let dir=tempfile::tempdir().unwrap();let foo=dir.path().join("foo");std::fs::create_dir(&foo).unwrap();
        let link=dir.path().join("link");std::os::unix::fs::symlink("foo/../bar",&link).unwrap();
        assert_eq!(realpath_without_open(&link),dir.path().join("bar"));
    }
    #[test]
    fn a_parent_retargeted_after_approval_is_refused_with_the_approval_reason()->std::io::Result<()> {
        let dir=tempfile::tempdir()?;let first=dir.path().join("first");let second=dir.path().join("second");std::fs::create_dir(&first)?;std::fs::create_dir(&second)?;
        let approved=std::fs::canonicalize(&first)?;
        let parent=dir.path().join("parent");std::os::unix::fs::symlink(&second,&parent)?;
        let error=FileMonitor::register("watch_1".to_owned(),"retarget".to_owned(),&parent.join("file"),FileEvent::Create,Some(&approved)).err().expect("register must fail").to_string();
        assert!(error.contains("parent directory changed during permission approval"),"{error}");Ok(())
    }
    #[test]
    fn a_target_whose_identity_changed_is_refused()->std::io::Result<()> {
        let dir=tempfile::tempdir()?;let real=dir.path().join("real");std::fs::create_dir(&real)?;let approved=std::fs::canonicalize(&real)?;
        let elsewhere=dir.path().join("elsewhere");std::fs::write(&elsewhere,b"x")?;
        // The watched target sits under the approved parent but is a symlink out to `elsewhere`, so
        // its resolved identity is not the approved parent plus basename.
        let target=real.join("target");std::os::unix::fs::symlink(&elsewhere,&target)?;
        let error=FileMonitor::register("watch_1".to_owned(),"identity".to_owned(),&target,FileEvent::Create,Some(&approved)).err().expect("register must fail").to_string();
        assert!(error.contains("target identity changed"),"{error}");Ok(())
    }
}
