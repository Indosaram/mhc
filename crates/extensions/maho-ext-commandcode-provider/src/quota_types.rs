#[derive(Clone, Debug, Default, PartialEq)]
pub struct QuotaSnapshot {
    pub credits: Option<String>,
    pub plan: Option<String>,
    pub usage: Option<String>,
    pub account: Option<String>,
    pub warning: Option<String>,
}
