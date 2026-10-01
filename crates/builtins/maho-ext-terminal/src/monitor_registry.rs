#[derive(Clone,Debug,Default)]
pub struct MonitorSnapshotEntry {
    pub id:String,
    pub monitor_id:Option<String>,
    pub description:String,
    pub paused:bool,
    pub started_at_ms:f64,
    pub command:Option<String>,
    pub filter:Option<String>,
    pub persistent:Option<bool>,
    pub deadline_ms:Option<f64>,
    pub fire_count:Option<usize>,
    pub last_fired_at_ms:Option<f64>,
    pub expires_at:Option<f64>,
    pub fire_window:Option<MonitorFireWindow>,
}

#[derive(Clone,Debug)]
pub struct MonitorFireWindow {pub start_ms:f64,pub count:usize}
