#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ScheduledTaskWeekday {
    #[serde(rename = "MO")]
    MO,
    #[serde(rename = "TU")]
    TU,
    #[serde(rename = "WE")]
    WE,
    #[serde(rename = "TH")]
    TH,
    #[serde(rename = "FR")]
    FR,
    #[serde(rename = "SA")]
    SA,
    #[serde(rename = "SU")]
    SU,
}
