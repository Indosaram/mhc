#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum RateLimitReachedType {
    #[serde(rename = "rate_limit_reached")]
    RateLimitReached,
    #[serde(rename = "workspace_owner_credits_depleted")]
    WorkspaceOwnerCreditsDepleted,
    #[serde(rename = "workspace_member_credits_depleted")]
    WorkspaceMemberCreditsDepleted,
    #[serde(rename = "workspace_owner_usage_limit_reached")]
    WorkspaceOwnerUsageLimitReached,
    #[serde(rename = "workspace_member_usage_limit_reached")]
    WorkspaceMemberUsageLimitReached,
}
