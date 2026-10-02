#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationError1 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::error_notification::ErrorNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationThreadStarted2 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_started_notification::ThreadStartedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationThreadStatusChanged3 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_status_changed_notification::ThreadStatusChangedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationThreadArchived4 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_archived_notification::ThreadArchivedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationThreadDeleted5 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_deleted_notification::ThreadDeletedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationThreadUnarchived6 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_unarchived_notification::ThreadUnarchivedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationThreadClosed7 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_closed_notification::ThreadClosedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationSkillsChanged8 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::skills_changed_notification::SkillsChangedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationThreadNameUpdated9 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_name_updated_notification::ThreadNameUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationThreadGoalUpdated10 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_goal_updated_notification::ThreadGoalUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationThreadGoalCleared11 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_goal_cleared_notification::ThreadGoalClearedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationThreadEnvironmentConnected12 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::environment_connection_notification::EnvironmentConnectionNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationThreadEnvironmentDisconnected13 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::environment_connection_notification::EnvironmentConnectionNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationThreadSettingsUpdated14 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_settings_updated_notification::ThreadSettingsUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationThreadTokenUsageUpdated15 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_token_usage_updated_notification::ThreadTokenUsageUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationTurnStarted16 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::turn_started_notification::TurnStartedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationHookStarted17 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::hook_started_notification::HookStartedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationTurnCompleted18 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::turn_completed_notification::TurnCompletedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationHookCompleted19 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::hook_completed_notification::HookCompletedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationTurnDiffUpdated20 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::turn_diff_updated_notification::TurnDiffUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationTurnPlanUpdated21 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::turn_plan_updated_notification::TurnPlanUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationItemStarted22 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::item_started_notification::ItemStartedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationItemAutoApprovalReviewStarted23 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::item_guardian_approval_review_started_notification::ItemGuardianApprovalReviewStartedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationItemAutoApprovalReviewCompleted24 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::item_guardian_approval_review_completed_notification::ItemGuardianApprovalReviewCompletedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationItemCompleted25 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::item_completed_notification::ItemCompletedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationRawResponseItemCompleted26 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::raw_response_item_completed_notification::RawResponseItemCompletedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationRawResponseCompleted27 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::raw_response_completed_notification::RawResponseCompletedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationItemAgentMessageDelta28 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::agent_message_delta_notification::AgentMessageDeltaNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationItemPlanDelta29 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::plan_delta_notification::PlanDeltaNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationCommandExecOutputDelta30 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::command_exec_output_delta_notification::CommandExecOutputDeltaNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationProcessOutputDelta31 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::process_output_delta_notification::ProcessOutputDeltaNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationProcessExited32 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::process_exited_notification::ProcessExitedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationItemCommandExecutionOutputDelta33 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::command_execution_output_delta_notification::CommandExecutionOutputDeltaNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationItemCommandExecutionTerminalInteraction34 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::terminal_interaction_notification::TerminalInteractionNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationItemFileChangeOutputDelta35 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::file_change_output_delta_notification::FileChangeOutputDeltaNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationItemFileChangePatchUpdated36 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::file_change_patch_updated_notification::FileChangePatchUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationServerRequestResolved37 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::server_request_resolved_notification::ServerRequestResolvedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationItemMcpToolCallProgress38 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::mcp_tool_call_progress_notification::McpToolCallProgressNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationMcpServerOauthLoginCompleted39 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::mcp_server_oauth_login_completed_notification::McpServerOauthLoginCompletedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationMcpServerStartupStatusUpdated40 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::mcp_server_status_updated_notification::McpServerStatusUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationAccountUpdated41 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::account_updated_notification::AccountUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationAccountRateLimitsUpdated42 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::account_rate_limits_updated_notification::AccountRateLimitsUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationAppListUpdated43 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::app_list_updated_notification::AppListUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationRemoteControlStatusChanged44 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::remote_control_status_changed_notification::RemoteControlStatusChangedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationExternalAgentConfigImportProgress45 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::external_agent_config_import_progress_notification::ExternalAgentConfigImportProgressNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationExternalAgentConfigImportCompleted46 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::external_agent_config_import_completed_notification::ExternalAgentConfigImportCompletedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationFsChanged47 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::fs_changed_notification::FsChangedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationItemReasoningSummaryTextDelta48 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::reasoning_summary_text_delta_notification::ReasoningSummaryTextDeltaNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationItemReasoningSummaryPartAdded49 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::reasoning_summary_part_added_notification::ReasoningSummaryPartAddedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationItemReasoningTextDelta50 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::reasoning_text_delta_notification::ReasoningTextDeltaNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationThreadCompacted51 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::context_compacted_notification::ContextCompactedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationModelRerouted52 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::model_rerouted_notification::ModelReroutedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationModelVerification53 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::model_verification_notification::ModelVerificationNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationTurnModerationMetadata54 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::turn_moderation_metadata_notification::TurnModerationMetadataNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationModelSafetyBufferingUpdated55 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::model_safety_buffering_updated_notification::ModelSafetyBufferingUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationWarning56 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::warning_notification::WarningNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationGuardianWarning57 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::guardian_warning_notification::GuardianWarningNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationDeprecationNotice58 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::deprecation_notice_notification::DeprecationNoticeNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationConfigWarning59 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::config_warning_notification::ConfigWarningNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationFuzzyFileSearchSessionUpdated60 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::fuzzy_file_search_session_updated_notification::FuzzyFileSearchSessionUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationFuzzyFileSearchSessionCompleted61 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::fuzzy_file_search_session_completed_notification::FuzzyFileSearchSessionCompletedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationThreadRealtimeStarted62 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_realtime_started_notification::ThreadRealtimeStartedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationThreadRealtimeItemAdded63 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_realtime_item_added_notification::ThreadRealtimeItemAddedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationThreadRealtimeTranscriptDelta64 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_realtime_transcript_delta_notification::ThreadRealtimeTranscriptDeltaNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationThreadRealtimeTranscriptDone65 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_realtime_transcript_done_notification::ThreadRealtimeTranscriptDoneNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationThreadRealtimeOutputAudioDelta66 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_realtime_output_audio_delta_notification::ThreadRealtimeOutputAudioDeltaNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationThreadRealtimeSdp67 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_realtime_sdp_notification::ThreadRealtimeSdpNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationThreadRealtimeError68 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_realtime_error_notification::ThreadRealtimeErrorNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationThreadRealtimeClosed69 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_realtime_closed_notification::ThreadRealtimeClosedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationWindowsWorldWritableWarning70 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::windows_world_writable_warning_notification::WindowsWorldWritableWarningNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationWindowsSandboxSetupCompleted71 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::windows_sandbox_setup_completed_notification::WindowsSandboxSetupCompletedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationAccountLoginCompleted72 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::account_login_completed_notification::AccountLoginCompletedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "method")]
pub enum ServerNotification {
    #[serde(rename = "error")]
    Error(Box<ServerNotificationError1>),
    #[serde(rename = "thread/started")]
    ThreadStarted(Box<ServerNotificationThreadStarted2>),
    #[serde(rename = "thread/status/changed")]
    ThreadStatusChanged(Box<ServerNotificationThreadStatusChanged3>),
    #[serde(rename = "thread/archived")]
    ThreadArchived(Box<ServerNotificationThreadArchived4>),
    #[serde(rename = "thread/deleted")]
    ThreadDeleted(Box<ServerNotificationThreadDeleted5>),
    #[serde(rename = "thread/unarchived")]
    ThreadUnarchived(Box<ServerNotificationThreadUnarchived6>),
    #[serde(rename = "thread/closed")]
    ThreadClosed(Box<ServerNotificationThreadClosed7>),
    #[serde(rename = "skills/changed")]
    SkillsChanged(Box<ServerNotificationSkillsChanged8>),
    #[serde(rename = "thread/name/updated")]
    ThreadNameUpdated(Box<ServerNotificationThreadNameUpdated9>),
    #[serde(rename = "thread/goal/updated")]
    ThreadGoalUpdated(Box<ServerNotificationThreadGoalUpdated10>),
    #[serde(rename = "thread/goal/cleared")]
    ThreadGoalCleared(Box<ServerNotificationThreadGoalCleared11>),
    #[serde(rename = "thread/environment/connected")]
    ThreadEnvironmentConnected(Box<ServerNotificationThreadEnvironmentConnected12>),
    #[serde(rename = "thread/environment/disconnected")]
    ThreadEnvironmentDisconnected(Box<ServerNotificationThreadEnvironmentDisconnected13>),
    #[serde(rename = "thread/settings/updated")]
    ThreadSettingsUpdated(Box<ServerNotificationThreadSettingsUpdated14>),
    #[serde(rename = "thread/tokenUsage/updated")]
    ThreadTokenUsageUpdated(Box<ServerNotificationThreadTokenUsageUpdated15>),
    #[serde(rename = "turn/started")]
    TurnStarted(Box<ServerNotificationTurnStarted16>),
    #[serde(rename = "hook/started")]
    HookStarted(Box<ServerNotificationHookStarted17>),
    #[serde(rename = "turn/completed")]
    TurnCompleted(Box<ServerNotificationTurnCompleted18>),
    #[serde(rename = "hook/completed")]
    HookCompleted(Box<ServerNotificationHookCompleted19>),
    #[serde(rename = "turn/diff/updated")]
    TurnDiffUpdated(Box<ServerNotificationTurnDiffUpdated20>),
    #[serde(rename = "turn/plan/updated")]
    TurnPlanUpdated(Box<ServerNotificationTurnPlanUpdated21>),
    #[serde(rename = "item/started")]
    ItemStarted(Box<ServerNotificationItemStarted22>),
    #[serde(rename = "item/autoApprovalReview/started")]
    ItemAutoApprovalReviewStarted(Box<ServerNotificationItemAutoApprovalReviewStarted23>),
    #[serde(rename = "item/autoApprovalReview/completed")]
    ItemAutoApprovalReviewCompleted(Box<ServerNotificationItemAutoApprovalReviewCompleted24>),
    #[serde(rename = "item/completed")]
    ItemCompleted(Box<ServerNotificationItemCompleted25>),
    #[serde(rename = "rawResponseItem/completed")]
    RawResponseItemCompleted(Box<ServerNotificationRawResponseItemCompleted26>),
    #[serde(rename = "rawResponse/completed")]
    RawResponseCompleted(Box<ServerNotificationRawResponseCompleted27>),
    #[serde(rename = "item/agentMessage/delta")]
    ItemAgentMessageDelta(Box<ServerNotificationItemAgentMessageDelta28>),
    #[serde(rename = "item/plan/delta")]
    ItemPlanDelta(Box<ServerNotificationItemPlanDelta29>),
    #[serde(rename = "command/exec/outputDelta")]
    CommandExecOutputDelta(Box<ServerNotificationCommandExecOutputDelta30>),
    #[serde(rename = "process/outputDelta")]
    ProcessOutputDelta(Box<ServerNotificationProcessOutputDelta31>),
    #[serde(rename = "process/exited")]
    ProcessExited(Box<ServerNotificationProcessExited32>),
    #[serde(rename = "item/commandExecution/outputDelta")]
    ItemCommandExecutionOutputDelta(Box<ServerNotificationItemCommandExecutionOutputDelta33>),
    #[serde(rename = "item/commandExecution/terminalInteraction")]
    ItemCommandExecutionTerminalInteraction(Box<ServerNotificationItemCommandExecutionTerminalInteraction34>),
    #[serde(rename = "item/fileChange/outputDelta")]
    ItemFileChangeOutputDelta(Box<ServerNotificationItemFileChangeOutputDelta35>),
    #[serde(rename = "item/fileChange/patchUpdated")]
    ItemFileChangePatchUpdated(Box<ServerNotificationItemFileChangePatchUpdated36>),
    #[serde(rename = "serverRequest/resolved")]
    ServerRequestResolved(Box<ServerNotificationServerRequestResolved37>),
    #[serde(rename = "item/mcpToolCall/progress")]
    ItemMcpToolCallProgress(Box<ServerNotificationItemMcpToolCallProgress38>),
    #[serde(rename = "mcpServer/oauthLogin/completed")]
    McpServerOauthLoginCompleted(Box<ServerNotificationMcpServerOauthLoginCompleted39>),
    #[serde(rename = "mcpServer/startupStatus/updated")]
    McpServerStartupStatusUpdated(Box<ServerNotificationMcpServerStartupStatusUpdated40>),
    #[serde(rename = "account/updated")]
    AccountUpdated(Box<ServerNotificationAccountUpdated41>),
    #[serde(rename = "account/rateLimits/updated")]
    AccountRateLimitsUpdated(Box<ServerNotificationAccountRateLimitsUpdated42>),
    #[serde(rename = "app/list/updated")]
    AppListUpdated(Box<ServerNotificationAppListUpdated43>),
    #[serde(rename = "remoteControl/status/changed")]
    RemoteControlStatusChanged(Box<ServerNotificationRemoteControlStatusChanged44>),
    #[serde(rename = "externalAgentConfig/import/progress")]
    ExternalAgentConfigImportProgress(Box<ServerNotificationExternalAgentConfigImportProgress45>),
    #[serde(rename = "externalAgentConfig/import/completed")]
    ExternalAgentConfigImportCompleted(Box<ServerNotificationExternalAgentConfigImportCompleted46>),
    #[serde(rename = "fs/changed")]
    FsChanged(Box<ServerNotificationFsChanged47>),
    #[serde(rename = "item/reasoning/summaryTextDelta")]
    ItemReasoningSummaryTextDelta(Box<ServerNotificationItemReasoningSummaryTextDelta48>),
    #[serde(rename = "item/reasoning/summaryPartAdded")]
    ItemReasoningSummaryPartAdded(Box<ServerNotificationItemReasoningSummaryPartAdded49>),
    #[serde(rename = "item/reasoning/textDelta")]
    ItemReasoningTextDelta(Box<ServerNotificationItemReasoningTextDelta50>),
    #[serde(rename = "thread/compacted")]
    ThreadCompacted(Box<ServerNotificationThreadCompacted51>),
    #[serde(rename = "model/rerouted")]
    ModelRerouted(Box<ServerNotificationModelRerouted52>),
    #[serde(rename = "model/verification")]
    ModelVerification(Box<ServerNotificationModelVerification53>),
    #[serde(rename = "turn/moderationMetadata")]
    TurnModerationMetadata(Box<ServerNotificationTurnModerationMetadata54>),
    #[serde(rename = "model/safetyBuffering/updated")]
    ModelSafetyBufferingUpdated(Box<ServerNotificationModelSafetyBufferingUpdated55>),
    #[serde(rename = "warning")]
    Warning(Box<ServerNotificationWarning56>),
    #[serde(rename = "guardianWarning")]
    GuardianWarning(Box<ServerNotificationGuardianWarning57>),
    #[serde(rename = "deprecationNotice")]
    DeprecationNotice(Box<ServerNotificationDeprecationNotice58>),
    #[serde(rename = "configWarning")]
    ConfigWarning(Box<ServerNotificationConfigWarning59>),
    #[serde(rename = "fuzzyFileSearch/sessionUpdated")]
    FuzzyFileSearchSessionUpdated(Box<ServerNotificationFuzzyFileSearchSessionUpdated60>),
    #[serde(rename = "fuzzyFileSearch/sessionCompleted")]
    FuzzyFileSearchSessionCompleted(Box<ServerNotificationFuzzyFileSearchSessionCompleted61>),
    #[serde(rename = "thread/realtime/started")]
    ThreadRealtimeStarted(Box<ServerNotificationThreadRealtimeStarted62>),
    #[serde(rename = "thread/realtime/itemAdded")]
    ThreadRealtimeItemAdded(Box<ServerNotificationThreadRealtimeItemAdded63>),
    #[serde(rename = "thread/realtime/transcript/delta")]
    ThreadRealtimeTranscriptDelta(Box<ServerNotificationThreadRealtimeTranscriptDelta64>),
    #[serde(rename = "thread/realtime/transcript/done")]
    ThreadRealtimeTranscriptDone(Box<ServerNotificationThreadRealtimeTranscriptDone65>),
    #[serde(rename = "thread/realtime/outputAudio/delta")]
    ThreadRealtimeOutputAudioDelta(Box<ServerNotificationThreadRealtimeOutputAudioDelta66>),
    #[serde(rename = "thread/realtime/sdp")]
    ThreadRealtimeSdp(Box<ServerNotificationThreadRealtimeSdp67>),
    #[serde(rename = "thread/realtime/error")]
    ThreadRealtimeError(Box<ServerNotificationThreadRealtimeError68>),
    #[serde(rename = "thread/realtime/closed")]
    ThreadRealtimeClosed(Box<ServerNotificationThreadRealtimeClosed69>),
    #[serde(rename = "windows/worldWritableWarning")]
    WindowsWorldWritableWarning(Box<ServerNotificationWindowsWorldWritableWarning70>),
    #[serde(rename = "windowsSandbox/setupCompleted")]
    WindowsSandboxSetupCompleted(Box<ServerNotificationWindowsSandboxSetupCompleted71>),
    #[serde(rename = "account/login/completed")]
    AccountLoginCompleted(Box<ServerNotificationAccountLoginCompleted72>),
}
