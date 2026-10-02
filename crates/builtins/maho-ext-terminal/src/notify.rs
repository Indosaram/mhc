use std::collections::HashSet;
use maho_pty::PtyExit;
use crate::settings::NotifyMode;
use crate::output_format::sanitize_terminal_output;
use crate::tools::spawn::describe_exit;
pub const NON_INTERACTIVE_MODES:[&str;2]=["print","json"];
pub const TERMINAL_NOTIFICATION_CUSTOM_TYPE:&str="senpi-terminal:notification";
pub const NOTICE_TAIL_MAX_CHARS:usize=2000;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum NotificationDelivery {Steer,FollowUp}
pub fn get_terminal_notification_delivery(notify_mode:NotifyMode,context_mode:Option<&str>,has_model:bool,force_wake:bool)->Option<NotificationDelivery> {let mode=context_mode?;if notify_mode==NotifyMode::Off||NON_INTERACTIVE_MODES.contains(&mode)||!has_model {return None;}Some(if notify_mode==NotifyMode::Wake||force_wake {NotificationDelivery::Steer} else {NotificationDelivery::FollowUp})}
pub fn build_notice(id:&str,exit:Option<&PtyExit>,output:&str)->String {
    let status=describe_exit(exit).unwrap_or_else(||"exited".to_owned());let code_text=exit.and_then(|exit|exit.exit_code).map(|code|format!(" (exit code {code})")).unwrap_or_default();
    let sanitized=sanitize_terminal_output(output);let tail=sanitized.trim_end();let units=tail.encode_utf16().collect::<Vec<_>>();
    let tail_section=if tail.is_empty() {String::new()} else {let truncated=units.len()>NOTICE_TAIL_MAX_CHARS;let shown=if truncated {String::from_utf16_lossy(&units[units.len()-NOTICE_TAIL_MAX_CHARS..])} else {tail.to_owned()};let note=if truncated {format!("\n[Final output truncated to the last {NOTICE_TAIL_MAX_CHARS} chars; the full history is still peekable.]")} else {String::new()};format!("\nFinal output:\n{shown}{note}")};
    format!("<system-reminder>Background terminal session {id} finished: {status}{code_text}.{tail_section}</system-reminder>")
}
#[derive(Default)]
pub struct TerminalNotifier {notified:HashSet<String>}
impl TerminalNotifier {
    pub fn notify_completion(&mut self,id:&str,delivery:Option<NotificationDelivery>,exit:Option<&PtyExit>,output:&str,mut send:impl FnMut(String,NotificationDelivery)) {if self.notified.contains(id) {return;}let Some(delivery)=delivery else {return;};self.notified.insert(id.to_owned());send(build_notice(id,exit,output),delivery);}
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn guards_and_modes() {for mode in NON_INTERACTIVE_MODES {assert_eq!(get_terminal_notification_delivery(NotifyMode::Wake,Some(mode),true,false),None);}assert_eq!(get_terminal_notification_delivery(NotifyMode::Off,Some("interactive"),true,true),None);assert_eq!(get_terminal_notification_delivery(NotifyMode::Wake,Some("interactive"),false,false),None);assert_eq!(get_terminal_notification_delivery(NotifyMode::NextTurn,Some("interactive"),true,false),Some(NotificationDelivery::FollowUp));assert_eq!(get_terminal_notification_delivery(NotifyMode::NextTurn,Some("interactive"),true,true),Some(NotificationDelivery::Steer));}
    #[test] fn once_only_after_deliverable() {let mut notifier=TerminalNotifier::default();let mut sent=Vec::new();notifier.notify_completion("bash_1",None,None,"",|text,_|sent.push(text));notifier.notify_completion("bash_1",Some(NotificationDelivery::Steer),None,"",|text,_|sent.push(text));notifier.notify_completion("bash_1",Some(NotificationDelivery::Steer),None,"",|text,_|sent.push(text));assert_eq!(sent.len(),1);}
}
