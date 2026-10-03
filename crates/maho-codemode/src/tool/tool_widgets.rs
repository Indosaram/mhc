pub fn code_point_prefix(text: &str, max_code_points: usize) -> String {
    text.chars().take(max_code_points).collect()
}

pub fn format_duration(milliseconds: f64) -> String {
    let total_seconds=(milliseconds.max(0.0)/1000.0).floor() as u64;
    if total_seconds<1 {return "<1s".into();}
    let seconds=total_seconds%60;
    let total_minutes=total_seconds/60;
    if total_minutes<1 {return format!("{seconds}s");}
    let minutes=total_minutes%60;
    let hours=total_minutes/60;
    if hours<1 {return if seconds==0 {format!("{minutes}m")} else {format!("{minutes}m {seconds}s")};}
    if minutes==0 {format!("{hours}h")} else {format!("{hours}h {minutes}m")}
}
