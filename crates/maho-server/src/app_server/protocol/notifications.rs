use serde::{Deserialize,Serialize};
use super::{account::*,base::JsonValue,fuzzy_search::*,terminal::ErrorNotification,thread_parity::{ThreadGoal,ThreadSettings}};
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadUnarchivedNotification {pub thread_id:String}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadGoalUpdatedNotification {pub thread_id:String,pub turn_id:Option<String>,pub goal:ThreadGoal}
pub type ThreadGoalClearedNotification=ThreadUnarchivedNotification;
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ThreadSettingsUpdatedNotification {pub thread_id:String,pub thread_settings:ThreadSettings}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct TurnDiffUpdatedNotification {pub thread_id:String,pub turn_id:String,pub diff:String}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(tag="method",content="params")]
pub enum AppServerPlanNotification {
    #[serde(rename="account/providerAccounts/updated")]ProviderAccountsUpdated(ProviderAccountsUpdatedNotification),
    #[serde(rename="account/providerAccounts/failover")]ProviderAccountFailover(ProviderAccountFailoverNotification),
    #[serde(rename="thread/unarchived")]ThreadUnarchived(ThreadUnarchivedNotification),
    #[serde(rename="thread/goal/updated")]ThreadGoalUpdated(ThreadGoalUpdatedNotification),
    #[serde(rename="thread/goal/cleared")]ThreadGoalCleared(ThreadGoalClearedNotification),
    #[serde(rename="thread/settings/updated")]ThreadSettingsUpdated(ThreadSettingsUpdatedNotification),
    #[serde(rename="turn/diff/updated")]TurnDiffUpdated(TurnDiffUpdatedNotification),
    #[serde(rename="fuzzyFileSearch/sessionUpdated")]FuzzyFileSearchSessionUpdated(FuzzyFileSearchSessionUpdatedNotification),
    #[serde(rename="fuzzyFileSearch/sessionCompleted")]FuzzyFileSearchSessionCompleted(FuzzyFileSearchSessionCompletedNotification),
}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(tag="method",content="params")]
pub enum ErrorServerNotification {#[serde(rename="error")]Error(ErrorNotification)}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(untagged)]
pub enum TypedServerNotification {Plan(Box<AppServerPlanNotification>),Error(ErrorServerNotification)}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
pub struct UntypedServerNotification {pub method:String,#[serde(default,skip_serializing_if="Option::is_none")]pub params:Option<JsonValue>}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(untagged)]
pub enum ServerNotification {Typed(TypedServerNotification),Untyped(UntypedServerNotification)}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ServerNotificationEnvelope {#[serde(flatten)]pub notification:ServerNotification,#[serde(default,skip_serializing_if="Option::is_none")]pub emitted_at_ms:Option<f64>}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct PopulatedServerNotificationEnvelope {#[serde(flatten)]pub notification:ServerNotification,pub emitted_at_ms:f64}
