use std::collections::{BTreeMap,BTreeSet};
use crate::{coordinator::resolve_detection,detectors::{collapse::{CollapseState,COLLAPSE_DETECTOR,create_collapse_state},control_leak::{ControlLeakDetector,ControlLeakState,corroborates_control_leak,create_control_leak_detector}},manager::{TtsrManager,TtsrMatchContext},prompts::{COLLAPSE_RULE_NAME,CONTROL_LEAK_RULE_NAME},types::*};
struct StreamTrack { collapse:CollapseState,leak:ControlLeakState }
pub struct WatchOutcome { pub resolution:Option<DetectionResolution>,pub rule_matches:Vec<TtsrRule> }
pub struct StreamWatcher { pub manager:TtsrManager,leak_detector:ControlLeakDetector,disabled_builtin:BTreeSet<String>,tracks:BTreeMap<String,StreamTrack> }
impl StreamWatcher {
    pub fn new(manager:TtsrManager,disabled_builtin_rules:&[String])->Self { Self { manager,leak_detector:create_control_leak_detector(),disabled_builtin:disabled_builtin_rules.iter().cloned().collect(),tracks:BTreeMap::new() } }
    pub fn reset(&mut self) { self.tracks.clear(); self.manager.reset_buffers(); }
    pub fn handle_delta(&mut self,source:TtsrStreamSource,stream_key:&str,delta:&str,generation:u64,tool_name:Option<&str>)->WatchOutcome {
        let label=match source { TtsrStreamSource::Text=>"text",TtsrStreamSource::Thinking=>"thinking",TtsrStreamSource::Tool=>"tool" };
        let track=self.tracks.entry(format!("{label}:{stream_key}")).or_insert_with(||StreamTrack { collapse:create_collapse_state(),leak:self.leak_detector.create_state() });
        let context=DetectorContext { source,stream_key:stream_key.into(),generation }; let units=delta.encode_utf16().collect::<Vec<_>>();
        let leak=if self.disabled_builtin.contains(CONTROL_LEAK_RULE_NAME) { None } else { self.leak_detector.check_delta(&mut track.leak,&units,&context) };
        let collapse=if self.disabled_builtin.contains(COLLAPSE_RULE_NAME) { None } else { COLLAPSE_DETECTOR.check_delta(&mut track.collapse,&units,&context) };
        let corroborated=collapse.as_ref().filter(|matched|track.leak.pending_evidence.as_ref().is_some_and(|evidence|corroborates_control_leak(evidence,matched.anomaly_start_offset,track.leak.current_offset)));
        let resolution=resolve_detection(leak.as_ref(),collapse.as_ref(),corroborated);
        let rule_matches=self.manager.check_delta(delta,&TtsrMatchContext { source,stream_key:stream_key.into(),tool_name:tool_name.map(str::to_owned),file_paths:None });
        WatchOutcome { resolution,rule_matches }
    }
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn leak_and_collapse_tracks_are_isolated() { let mut watcher=StreamWatcher::new(TtsrManager::new(Default::default()),&[]); watcher.handle_delta(TtsrStreamSource::Text,"a","<s> <s>",1,None); let quiet=watcher.handle_delta(TtsrStreamSource::Text,"b","<s>",1,None); let loud=watcher.handle_delta(TtsrStreamSource::Text,"a"," <s>",1,None); assert!(quiet.resolution.is_none()); assert_eq!(loud.resolution.unwrap().owner,DetectionOwner::ControlTokenLeak); }
    #[test] fn disabled_detectors_do_not_fire() { let mut watcher=StreamWatcher::new(TtsrManager::new(Default::default()),&[COLLAPSE_RULE_NAME.into(),CONTROL_LEAK_RULE_NAME.into()]); let result=watcher.handle_delta(TtsrStreamSource::Text,"a",&format!("<s> <s> <s>{}","!".repeat(300)),1,None); assert!(result.resolution.is_none()); }
    #[test] fn reset_clears_latched_detection() { let mut watcher=StreamWatcher::new(TtsrManager::new(Default::default()),&[]); let first=watcher.handle_delta(TtsrStreamSource::Text,"a","<s> <s> <s>",1,None); assert!(first.resolution.is_some()); watcher.reset(); let result=watcher.handle_delta(TtsrStreamSource::Text,"a","normal",2,None); assert!(result.resolution.is_none()); }
    #[test] fn adjacent_collapse_corroborates_control_leak() { let mut watcher=StreamWatcher::new(TtsrManager::new(Default::default()),&[]); let result=watcher.handle_delta(TtsrStreamSource::Text,"a",&format!("<|close|>{}","!".repeat(300)),1,None); assert_eq!(result.resolution.unwrap().owner,DetectionOwner::ControlTokenLeak); }
}
