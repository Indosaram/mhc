#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AccountTokenUsageSummary {
    #[serde(rename = "lifetimeTokens", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub lifetime_tokens: Option<i64>,
    #[serde(rename = "peakDailyTokens", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub peak_daily_tokens: Option<i64>,
    #[serde(rename = "longestRunningTurnSec", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub longest_running_turn_sec: Option<i64>,
    #[serde(rename = "currentStreakDays", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub current_streak_days: Option<i64>,
    #[serde(rename = "longestStreakDays", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub longest_streak_days: Option<i64>,
}
