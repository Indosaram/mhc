use crate::terminal_manifest_model::{TerminalManifestCheckpoint,ManifestMonitor};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum DetachedChange {Created,Replaced,Modified,Gone}
pub fn classify_detached_change(saved:&TerminalManifestCheckpoint,live:&TerminalManifestCheckpoint)->Option<DetachedChange> {
    if !saved.present {return if live.present {Some(DetachedChange::Created)} else {None};}
    if !live.present {return Some(DetachedChange::Gone);}
    if live.dev!=saved.dev||live.ino!=saved.ino {return Some(DetachedChange::Replaced);}
    if live.mtime_ms!=saved.mtime_ms||live.size!=saved.size||live.digest!=saved.digest {return Some(DetachedChange::Modified);}
    None
}
pub fn remaining_ms(monitor:&ManifestMonitor,now:f64)->f64 {monitor.expires_at.map_or(300_000.0,|expiry|(expiry-now).max(1.0))}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn detached_change_precedence_and_digest_only_modification() {let saved=TerminalManifestCheckpoint {dev:1.0,ino:2.0,size:4.0,mtime_ms:5.0,digest:"old".to_owned(),present:true};let mut live=saved.clone();assert_eq!(classify_detached_change(&saved,&live),None);live.digest="new".to_owned();assert_eq!(classify_detached_change(&saved,&live),Some(DetachedChange::Modified));live.ino=3.0;assert_eq!(classify_detached_change(&saved,&live),Some(DetachedChange::Replaced));live.present=false;assert_eq!(classify_detached_change(&saved,&live),Some(DetachedChange::Gone));let mut absent=saved;absent.present=false;assert_eq!(classify_detached_change(&absent,&live),None);live.present=true;assert_eq!(classify_detached_change(&absent,&live),Some(DetachedChange::Created));}
}
