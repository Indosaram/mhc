use crate::monitor_registry::MonitorSnapshotEntry;
pub const MONITOR_STATUS_KEY:&str="monitors";
const MAX_STATUS_LENGTH:usize=48;

fn units(text:&str)->usize {text.encode_utf16().count()}
fn truncate_end(text:&str,max:usize)->String {
    if units(text)<=max {return text.to_owned();}
    let text:Vec<_>=text.encode_utf16().take(max.saturating_sub(1)).collect();
    format!("{}\u{2026}",String::from_utf16_lossy(&text))
}
fn pack_descriptions(names:&[&str],budget:usize)->String {
    for kept in (1..=names.len()).rev() {
        let hidden=names.len()-kept;let tail=if hidden>0 {format!(" +{hidden} more")} else {String::new()};
        let joined=names[..kept].join(", ");if units(&joined)+units(&tail)<=budget {return joined+&tail;}
    }
    let tail=if names.len()>1 {format!(" +{} more",names.len()-1)} else {String::new()};
    truncate_end(names.first().copied().unwrap_or(""),budget.saturating_sub(units(&tail)).max(1))+&tail
}
pub fn format_elapsed_seconds(value:f64)->String {
    let seconds=value.trunc().max(0.0);if seconds<60.0 {return format!("{seconds:.0}s");}
    let minutes=(seconds/60.0).trunc();if minutes<60.0 {return format!("{minutes:.0}m");}
    let hours=(minutes/60.0).trunc();let remainder=minutes%60.0;
    if hours>=24.0 {return format!("{:.0}d {:.0}h {remainder:.0}m",(hours/24.0).trunc(),hours%24.0);}
    if remainder==0.0 {format!("{hours:.0}h")} else {format!("{hours:.0}h {remainder:.0}m")}
}
pub fn monitor_elapsed_seconds(snapshot:&[MonitorSnapshotEntry],now_ms:f64)->f64 {
    let oldest=snapshot.iter().map(|e|e.started_at_ms).fold(f64::INFINITY,f64::min);
    if !oldest.is_finite() {return 0.0;}
    (((now_ms-oldest)/1000.0+0.5).floor()).max(0.0)
}
pub fn format_monitor_status(snapshot:&[MonitorSnapshotEntry],now_ms:f64)->Option<String> {
    if snapshot.is_empty() {return None;}
    let paused=snapshot.iter().filter(|e|e.paused).count();
    let paused_part=if paused==0 {String::new()} else if paused==snapshot.len() {", muted".to_owned()} else {format!(", {paused} muted")};
    let soonest=snapshot.iter().filter_map(|e|e.expires_at).map(|expiry|expiry-now_ms).fold(f64::INFINITY,f64::min);
    let expiry=if soonest.is_finite() && soonest<86_400_000.0 {format!(" (expires in {:.0}d)",(soonest/86_400_000.0).ceil().max(1.0))} else {String::new()};
    let suffix=format!(" ({}{paused_part}){expiry}",format_elapsed_seconds(monitor_elapsed_seconds(snapshot,now_ms)));
    let head=if snapshot.len()==1 {"\u{25c9} watching ".to_owned()} else {format!("\u{25c9} watching {}: ",snapshot.len())};
    let budget=MAX_STATUS_LENGTH.saturating_sub(units(&head)+units(&suffix));
    let description=if snapshot.len()==1 {truncate_end(&snapshot[0].description,budget)} else {pack_descriptions(&snapshot.iter().map(|e|e.description.as_str()).collect::<Vec<_>>(),budget)};
    Some(head+&description+&suffix)
}
/// Upstream `registerTerminalExtension`'s status ticker render: a plain status passes through
/// unless the session is a TUI, where it is wrapped as `theme.bg("selectedBg", theme.fg("text", status))`
/// via the shared guest `Theme::{fg,bg}` methods.
pub fn render_monitor_status(mode:&str,theme:&maho_ext_api::types::Theme,status:Option<&str>)->Option<String> {
    let Some(status)=status else {return None;};
    if mode!="tui" {return Some(status.to_owned());}
    Some(theme.bg(maho_ext_api::types::ThemeBg::SelectedBg,&theme.fg(maho_ext_api::types::ThemeColor::Text,status)))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(description:&str)->MonitorSnapshotEntry {MonitorSnapshotEntry {description:description.to_owned(),started_at_ms:1_000_000.0,..Default::default()}}
    #[test] fn empty_clears_status() {assert_eq!(format_monitor_status(&[],1_000_000.0),None);}
    #[test] fn single_watch_elapsed() {assert_eq!(format_monitor_status(&[entry("errors in deploy.log")],1_005_000.0).as_deref(),Some("\u{25c9} watching errors in deploy.log (5s)"));}
    #[test] fn longer_elapsed_labels() {assert_eq!(format_elapsed_seconds(180.0),"3m");assert_eq!(format_elapsed_seconds(9000.0),"2h 30m");assert_eq!(format_elapsed_seconds(93780.0),"1d 2h 3m");}
    #[test] fn elapsed_advances_same_snapshot() {let snapshot=[entry("deploy errors")];assert_eq!(monitor_elapsed_seconds(&snapshot,1_005_000.0),5.0);assert_eq!(monitor_elapsed_seconds(&snapshot,1_006_000.0),6.0);}
    #[test] fn backwards_clock_clamps() {assert_eq!(monitor_elapsed_seconds(&[entry("deploy errors")],995_000.0),0.0);}
    #[test] fn whole_names_oldest_watch() {let mut newer=entry("webpack");newer.started_at_ms+=60_000.0;assert_eq!(format_monitor_status(&[entry("deploy errors"),newer],1_180_000.0).as_deref(),Some("\u{25c9} watching 2: deploy errors, webpack (3m)"));}
    #[test] fn overflow_counter_keeps_whole_names() {assert_eq!(format_monitor_status(&[entry("errors in deploy.log"),entry("integration test output on ci runner four"),entry("webpack rebuild")],1_000_000.0).as_deref(),Some("\u{25c9} watching 3: errors in deploy.log +2 more (0s)"));}
    #[test] fn first_name_overflow_keeps_count() {let text=format_monitor_status(&[entry(&"a".repeat(60)),entry("b"),entry("c"),entry("d")],1_000_000.0).expect("status");assert!(text.contains("watching 4:") && text.contains("+3 more") && text.contains('\u{2026}'));assert!(units(&text)<=48);}
    #[test] fn durable_under_day_warns() {let mut durable=entry("deploy errors");durable.expires_at=Some(4_600_000.0);assert_eq!(format_monitor_status(&[durable],1_000_000.0).as_deref(),Some("\u{25c9} watching deploy errors (0s) (expires in 1d)"));}
    #[test] fn durable_days_left_quiet() {let mut durable=entry("deploy errors");durable.expires_at=Some(433_000_000.0);assert_eq!(format_monitor_status(&[durable],1_000_000.0).as_deref(),Some("\u{25c9} watching deploy errors (0s)"));}
    #[test] fn ephemeral_no_expiry() {assert_eq!(format_monitor_status(&[entry("deploy errors")],1_000_000.0).as_deref(),Some("\u{25c9} watching deploy errors (0s)"));}
    #[test] fn paused_marker_survives_truncation() {let mut a=entry("a");a.paused=true;let mut b=entry("b");b.paused=true;assert_eq!(format_monitor_status(&[a,b],1_060_000.0).as_deref(),Some("\u{25c9} watching 2: a, b (1m, muted)"));}
    #[test] fn status_is_plain_outside_tui_and_themed_inside_it() {
        use maho_ext_api::types::Theme;
        let theme=Theme {colors:[("text".to_owned(),"\x1b[38;2;1;2;3m".to_owned())].into(),backgrounds:[("selectedBg".to_owned(),"\x1b[48;2;4;5;6m".to_owned())].into(),..Default::default()};
        assert_eq!(render_monitor_status("print",&theme,Some("\u{25c9} watching x (1s)")).as_deref(),Some("\u{25c9} watching x (1s)"));
        assert_eq!(render_monitor_status("tui",&theme,Some("watch")).as_deref(),Some("\x1b[48;2;4;5;6m\x1b[38;2;1;2;3mwatch\x1b[39m\x1b[49m"));
        assert_eq!(render_monitor_status("tui",&theme,None),None);
        let unthemed=Theme::default();
        assert_eq!(render_monitor_status("tui",&unthemed,Some("watch")).as_deref(),Some("\x1b[49m\x1b[39mwatch\x1b[39m\x1b[49m"));
    }
}
