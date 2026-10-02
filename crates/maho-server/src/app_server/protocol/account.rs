use serde::{Deserialize,Serialize};
use std::collections::BTreeMap;
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum PlanType {Free,Go,Plus,Pro,Prolite,Team,SelfServeBusinessUsageBased,Business,EnterpriseCbpUsageBased,Enterprise,Edu,Unknown}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(tag="type",rename_all="camelCase",rename_all_fields="camelCase")]
pub enum Account {ApiKey,Chatgpt {email:Option<String>,plan_type:PlanType},AmazonBedrock {uses_codex_managed_credentials:bool}}
#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct AccountReadParams {#[serde(default,skip_serializing_if="Option::is_none")]pub refresh_token:Option<bool>}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct AccountReadResponse {pub account:Option<Account>,pub requires_openai_auth:bool}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum ProviderAccountSource {Login,Import,Env}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ProviderAccount {pub name:String,pub source:ProviderAccountSource,pub blocked:bool,pub pinned:bool,#[serde(default,skip_serializing_if="Option::is_none")]pub display_name:Option<String>}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct ProviderAccountsReadParams {pub provider:String}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct ProviderAccountsReadResponse {pub provider:String,pub accounts:Vec<ProviderAccount>}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct ProviderAccountsPinParams {pub provider:String,pub name:Option<String>}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct ProviderAccountsRemoveParams {pub provider:String,pub name:String}
pub type ProviderAccountsUpdatedNotification=ProviderAccountsReadParams;
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
pub struct ProviderAccountFailoverNotification {pub provider:String,pub from:String,pub to:String,pub reason:String}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct RateLimitWindow {pub used_percent:f64,pub window_duration_mins:Option<f64>,pub resets_at:Option<f64>}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct CreditsSnapshot {pub has_credits:bool,pub unlimited:bool,pub balance:Option<String>}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct SpendControlLimitSnapshot {pub limit:String,pub used:String,pub remaining_percent:f64,pub resets_at:f64}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum RateLimitReachedType {RateLimitReached,WorkspaceOwnerCreditsDepleted,WorkspaceMemberCreditsDepleted,WorkspaceOwnerUsageLimitReached,WorkspaceMemberUsageLimitReached}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct RateLimitSnapshot {
    pub limit_id:Option<String>,pub limit_name:Option<String>,pub primary:Option<RateLimitWindow>,pub secondary:Option<RateLimitWindow>,pub credits:Option<CreditsSnapshot>,
    pub individual_limit:Option<SpendControlLimitSnapshot>,pub spend_control_reached:Option<bool>,pub plan_type:Option<PlanType>,pub rate_limit_reached_type:Option<RateLimitReachedType>,
}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub enum RateLimitResetType {CodexRateLimits,Unknown}
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum RateLimitResetCreditStatus {Available,Redeeming,Redeemed,Unknown}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct RateLimitResetCredit {pub id:String,pub reset_type:RateLimitResetType,pub status:RateLimitResetCreditStatus,pub granted_at:f64,pub expires_at:Option<f64>,pub title:Option<String>,pub description:Option<String>}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct RateLimitResetCreditsSummary {pub available_count:f64,pub credits:Option<Vec<RateLimitResetCredit>>}
pub type AccountRateLimitsReadParams=();
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct AccountRateLimitsReadResponse {pub rate_limits:RateLimitSnapshot,pub rate_limits_by_limit_id:Option<BTreeMap<String,RateLimitSnapshot>>,pub rate_limit_reset_credits:Option<RateLimitResetCreditsSummary>}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct AccountTokenUsageSummary {pub lifetime_tokens:Option<f64>,pub peak_daily_tokens:Option<f64>,pub longest_running_turn_sec:Option<f64>,pub current_streak_days:Option<f64>,pub longest_streak_days:Option<f64>}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct AccountTokenUsageDailyBucket {pub start_date:String,pub tokens:f64}
pub type AccountUsageReadParams=();
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct AccountUsageReadResponse {pub summary:AccountTokenUsageSummary,pub daily_usage_buckets:Option<Vec<AccountTokenUsageDailyBucket>>}
