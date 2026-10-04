#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01Error2 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::error_notification::ErrorNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ThreadStarted3 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_started_notification::ThreadStartedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ThreadStatusChanged4 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_status_changed_notification::ThreadStatusChangedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ThreadArchived5 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_archived_notification::ThreadArchivedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ThreadDeleted6 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_deleted_notification::ThreadDeletedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ThreadUnarchived7 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_unarchived_notification::ThreadUnarchivedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ThreadClosed8 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_closed_notification::ThreadClosedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01SkillsChanged9 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::skills_changed_notification::SkillsChangedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ThreadNameUpdated10 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_name_updated_notification::ThreadNameUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ThreadGoalUpdated11 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_goal_updated_notification::ThreadGoalUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ThreadGoalCleared12 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_goal_cleared_notification::ThreadGoalClearedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ThreadEnvironmentConnected13 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::environment_connection_notification::EnvironmentConnectionNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ThreadEnvironmentDisconnected14 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::environment_connection_notification::EnvironmentConnectionNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ThreadSettingsUpdated15 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_settings_updated_notification::ThreadSettingsUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ThreadTokenUsageUpdated16 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_token_usage_updated_notification::ThreadTokenUsageUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01TurnStarted17 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::turn_started_notification::TurnStartedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01HookStarted18 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::hook_started_notification::HookStartedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01TurnCompleted19 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::turn_completed_notification::TurnCompletedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01HookCompleted20 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::hook_completed_notification::HookCompletedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01TurnDiffUpdated21 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::turn_diff_updated_notification::TurnDiffUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01TurnPlanUpdated22 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::turn_plan_updated_notification::TurnPlanUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ItemStarted23 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::item_started_notification::ItemStartedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ItemAutoApprovalReviewStarted24 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::item_guardian_approval_review_started_notification::ItemGuardianApprovalReviewStartedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ItemAutoApprovalReviewCompleted25 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::item_guardian_approval_review_completed_notification::ItemGuardianApprovalReviewCompletedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ItemCompleted26 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::item_completed_notification::ItemCompletedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01RawResponseItemCompleted27 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::raw_response_item_completed_notification::RawResponseItemCompletedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01RawResponseCompleted28 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::raw_response_completed_notification::RawResponseCompletedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ItemAgentMessageDelta29 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::agent_message_delta_notification::AgentMessageDeltaNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ItemPlanDelta30 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::plan_delta_notification::PlanDeltaNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01CommandExecOutputDelta31 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::command_exec_output_delta_notification::CommandExecOutputDeltaNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ProcessOutputDelta32 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::process_output_delta_notification::ProcessOutputDeltaNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ProcessExited33 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::process_exited_notification::ProcessExitedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ItemCommandExecutionOutputDelta34 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::command_execution_output_delta_notification::CommandExecutionOutputDeltaNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ItemCommandExecutionTerminalInteraction35 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::terminal_interaction_notification::TerminalInteractionNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ItemFileChangeOutputDelta36 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::file_change_output_delta_notification::FileChangeOutputDeltaNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ItemFileChangePatchUpdated37 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::file_change_patch_updated_notification::FileChangePatchUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ServerRequestResolved38 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::server_request_resolved_notification::ServerRequestResolvedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ItemMcpToolCallProgress39 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::mcp_tool_call_progress_notification::McpToolCallProgressNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01McpServerOauthLoginCompleted40 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::mcp_server_oauth_login_completed_notification::McpServerOauthLoginCompletedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01McpServerStartupStatusUpdated41 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::mcp_server_status_updated_notification::McpServerStatusUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01AccountUpdated42 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::account_updated_notification::AccountUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01AccountRateLimitsUpdated43 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::account_rate_limits_updated_notification::AccountRateLimitsUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01AppListUpdated44 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::app_list_updated_notification::AppListUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01RemoteControlStatusChanged45 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::remote_control_status_changed_notification::RemoteControlStatusChangedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ExternalAgentConfigImportProgress46 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::external_agent_config_import_progress_notification::ExternalAgentConfigImportProgressNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ExternalAgentConfigImportCompleted47 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::external_agent_config_import_completed_notification::ExternalAgentConfigImportCompletedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01FsChanged48 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::fs_changed_notification::FsChangedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ItemReasoningSummaryTextDelta49 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::reasoning_summary_text_delta_notification::ReasoningSummaryTextDeltaNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ItemReasoningSummaryPartAdded50 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::reasoning_summary_part_added_notification::ReasoningSummaryPartAddedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ItemReasoningTextDelta51 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::reasoning_text_delta_notification::ReasoningTextDeltaNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ThreadCompacted52 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::context_compacted_notification::ContextCompactedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ModelRerouted53 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::model_rerouted_notification::ModelReroutedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ModelVerification54 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::model_verification_notification::ModelVerificationNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01TurnModerationMetadata55 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::turn_moderation_metadata_notification::TurnModerationMetadataNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ModelSafetyBufferingUpdated56 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::model_safety_buffering_updated_notification::ModelSafetyBufferingUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01Warning57 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::warning_notification::WarningNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01GuardianWarning58 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::guardian_warning_notification::GuardianWarningNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01DeprecationNotice59 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::deprecation_notice_notification::DeprecationNoticeNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ConfigWarning60 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::config_warning_notification::ConfigWarningNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01FuzzyFileSearchSessionUpdated61 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::fuzzy_file_search_session_updated_notification::FuzzyFileSearchSessionUpdatedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01FuzzyFileSearchSessionCompleted62 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::fuzzy_file_search_session_completed_notification::FuzzyFileSearchSessionCompletedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ThreadRealtimeStarted63 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_realtime_started_notification::ThreadRealtimeStartedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ThreadRealtimeItemAdded64 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_realtime_item_added_notification::ThreadRealtimeItemAddedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ThreadRealtimeTranscriptDelta65 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_realtime_transcript_delta_notification::ThreadRealtimeTranscriptDeltaNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ThreadRealtimeTranscriptDone66 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_realtime_transcript_done_notification::ThreadRealtimeTranscriptDoneNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ThreadRealtimeOutputAudioDelta67 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_realtime_output_audio_delta_notification::ThreadRealtimeOutputAudioDeltaNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ThreadRealtimeSdp68 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_realtime_sdp_notification::ThreadRealtimeSdpNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ThreadRealtimeError69 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_realtime_error_notification::ThreadRealtimeErrorNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01ThreadRealtimeClosed70 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_realtime_closed_notification::ThreadRealtimeClosedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01WindowsWorldWritableWarning71 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::windows_world_writable_warning_notification::WindowsWorldWritableWarningNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01WindowsSandboxSetupCompleted72 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::windows_sandbox_setup_completed_notification::WindowsSandboxSetupCompletedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelopeDetails01AccountLoginCompleted73 {
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::account_login_completed_notification::AccountLoginCompletedNotification>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "method")]
pub enum ServerNotificationEnvelopeDetails01 {
    #[serde(rename = "error")]
    Error(Box<ServerNotificationEnvelopeDetails01Error2>),
    #[serde(rename = "thread/started")]
    ThreadStarted(Box<ServerNotificationEnvelopeDetails01ThreadStarted3>),
    #[serde(rename = "thread/status/changed")]
    ThreadStatusChanged(Box<ServerNotificationEnvelopeDetails01ThreadStatusChanged4>),
    #[serde(rename = "thread/archived")]
    ThreadArchived(Box<ServerNotificationEnvelopeDetails01ThreadArchived5>),
    #[serde(rename = "thread/deleted")]
    ThreadDeleted(Box<ServerNotificationEnvelopeDetails01ThreadDeleted6>),
    #[serde(rename = "thread/unarchived")]
    ThreadUnarchived(Box<ServerNotificationEnvelopeDetails01ThreadUnarchived7>),
    #[serde(rename = "thread/closed")]
    ThreadClosed(Box<ServerNotificationEnvelopeDetails01ThreadClosed8>),
    #[serde(rename = "skills/changed")]
    SkillsChanged(Box<ServerNotificationEnvelopeDetails01SkillsChanged9>),
    #[serde(rename = "thread/name/updated")]
    ThreadNameUpdated(Box<ServerNotificationEnvelopeDetails01ThreadNameUpdated10>),
    #[serde(rename = "thread/goal/updated")]
    ThreadGoalUpdated(Box<ServerNotificationEnvelopeDetails01ThreadGoalUpdated11>),
    #[serde(rename = "thread/goal/cleared")]
    ThreadGoalCleared(Box<ServerNotificationEnvelopeDetails01ThreadGoalCleared12>),
    #[serde(rename = "thread/environment/connected")]
    ThreadEnvironmentConnected(Box<ServerNotificationEnvelopeDetails01ThreadEnvironmentConnected13>),
    #[serde(rename = "thread/environment/disconnected")]
    ThreadEnvironmentDisconnected(Box<ServerNotificationEnvelopeDetails01ThreadEnvironmentDisconnected14>),
    #[serde(rename = "thread/settings/updated")]
    ThreadSettingsUpdated(Box<ServerNotificationEnvelopeDetails01ThreadSettingsUpdated15>),
    #[serde(rename = "thread/tokenUsage/updated")]
    ThreadTokenUsageUpdated(Box<ServerNotificationEnvelopeDetails01ThreadTokenUsageUpdated16>),
    #[serde(rename = "turn/started")]
    TurnStarted(Box<ServerNotificationEnvelopeDetails01TurnStarted17>),
    #[serde(rename = "hook/started")]
    HookStarted(Box<ServerNotificationEnvelopeDetails01HookStarted18>),
    #[serde(rename = "turn/completed")]
    TurnCompleted(Box<ServerNotificationEnvelopeDetails01TurnCompleted19>),
    #[serde(rename = "hook/completed")]
    HookCompleted(Box<ServerNotificationEnvelopeDetails01HookCompleted20>),
    #[serde(rename = "turn/diff/updated")]
    TurnDiffUpdated(Box<ServerNotificationEnvelopeDetails01TurnDiffUpdated21>),
    #[serde(rename = "turn/plan/updated")]
    TurnPlanUpdated(Box<ServerNotificationEnvelopeDetails01TurnPlanUpdated22>),
    #[serde(rename = "item/started")]
    ItemStarted(Box<ServerNotificationEnvelopeDetails01ItemStarted23>),
    #[serde(rename = "item/autoApprovalReview/started")]
    ItemAutoApprovalReviewStarted(Box<ServerNotificationEnvelopeDetails01ItemAutoApprovalReviewStarted24>),
    #[serde(rename = "item/autoApprovalReview/completed")]
    ItemAutoApprovalReviewCompleted(Box<ServerNotificationEnvelopeDetails01ItemAutoApprovalReviewCompleted25>),
    #[serde(rename = "item/completed")]
    ItemCompleted(Box<ServerNotificationEnvelopeDetails01ItemCompleted26>),
    #[serde(rename = "rawResponseItem/completed")]
    RawResponseItemCompleted(Box<ServerNotificationEnvelopeDetails01RawResponseItemCompleted27>),
    #[serde(rename = "rawResponse/completed")]
    RawResponseCompleted(Box<ServerNotificationEnvelopeDetails01RawResponseCompleted28>),
    #[serde(rename = "item/agentMessage/delta")]
    ItemAgentMessageDelta(Box<ServerNotificationEnvelopeDetails01ItemAgentMessageDelta29>),
    #[serde(rename = "item/plan/delta")]
    ItemPlanDelta(Box<ServerNotificationEnvelopeDetails01ItemPlanDelta30>),
    #[serde(rename = "command/exec/outputDelta")]
    CommandExecOutputDelta(Box<ServerNotificationEnvelopeDetails01CommandExecOutputDelta31>),
    #[serde(rename = "process/outputDelta")]
    ProcessOutputDelta(Box<ServerNotificationEnvelopeDetails01ProcessOutputDelta32>),
    #[serde(rename = "process/exited")]
    ProcessExited(Box<ServerNotificationEnvelopeDetails01ProcessExited33>),
    #[serde(rename = "item/commandExecution/outputDelta")]
    ItemCommandExecutionOutputDelta(Box<ServerNotificationEnvelopeDetails01ItemCommandExecutionOutputDelta34>),
    #[serde(rename = "item/commandExecution/terminalInteraction")]
    ItemCommandExecutionTerminalInteraction(Box<ServerNotificationEnvelopeDetails01ItemCommandExecutionTerminalInteraction35>),
    #[serde(rename = "item/fileChange/outputDelta")]
    ItemFileChangeOutputDelta(Box<ServerNotificationEnvelopeDetails01ItemFileChangeOutputDelta36>),
    #[serde(rename = "item/fileChange/patchUpdated")]
    ItemFileChangePatchUpdated(Box<ServerNotificationEnvelopeDetails01ItemFileChangePatchUpdated37>),
    #[serde(rename = "serverRequest/resolved")]
    ServerRequestResolved(Box<ServerNotificationEnvelopeDetails01ServerRequestResolved38>),
    #[serde(rename = "item/mcpToolCall/progress")]
    ItemMcpToolCallProgress(Box<ServerNotificationEnvelopeDetails01ItemMcpToolCallProgress39>),
    #[serde(rename = "mcpServer/oauthLogin/completed")]
    McpServerOauthLoginCompleted(Box<ServerNotificationEnvelopeDetails01McpServerOauthLoginCompleted40>),
    #[serde(rename = "mcpServer/startupStatus/updated")]
    McpServerStartupStatusUpdated(Box<ServerNotificationEnvelopeDetails01McpServerStartupStatusUpdated41>),
    #[serde(rename = "account/updated")]
    AccountUpdated(Box<ServerNotificationEnvelopeDetails01AccountUpdated42>),
    #[serde(rename = "account/rateLimits/updated")]
    AccountRateLimitsUpdated(Box<ServerNotificationEnvelopeDetails01AccountRateLimitsUpdated43>),
    #[serde(rename = "app/list/updated")]
    AppListUpdated(Box<ServerNotificationEnvelopeDetails01AppListUpdated44>),
    #[serde(rename = "remoteControl/status/changed")]
    RemoteControlStatusChanged(Box<ServerNotificationEnvelopeDetails01RemoteControlStatusChanged45>),
    #[serde(rename = "externalAgentConfig/import/progress")]
    ExternalAgentConfigImportProgress(Box<ServerNotificationEnvelopeDetails01ExternalAgentConfigImportProgress46>),
    #[serde(rename = "externalAgentConfig/import/completed")]
    ExternalAgentConfigImportCompleted(Box<ServerNotificationEnvelopeDetails01ExternalAgentConfigImportCompleted47>),
    #[serde(rename = "fs/changed")]
    FsChanged(Box<ServerNotificationEnvelopeDetails01FsChanged48>),
    #[serde(rename = "item/reasoning/summaryTextDelta")]
    ItemReasoningSummaryTextDelta(Box<ServerNotificationEnvelopeDetails01ItemReasoningSummaryTextDelta49>),
    #[serde(rename = "item/reasoning/summaryPartAdded")]
    ItemReasoningSummaryPartAdded(Box<ServerNotificationEnvelopeDetails01ItemReasoningSummaryPartAdded50>),
    #[serde(rename = "item/reasoning/textDelta")]
    ItemReasoningTextDelta(Box<ServerNotificationEnvelopeDetails01ItemReasoningTextDelta51>),
    #[serde(rename = "thread/compacted")]
    ThreadCompacted(Box<ServerNotificationEnvelopeDetails01ThreadCompacted52>),
    #[serde(rename = "model/rerouted")]
    ModelRerouted(Box<ServerNotificationEnvelopeDetails01ModelRerouted53>),
    #[serde(rename = "model/verification")]
    ModelVerification(Box<ServerNotificationEnvelopeDetails01ModelVerification54>),
    #[serde(rename = "turn/moderationMetadata")]
    TurnModerationMetadata(Box<ServerNotificationEnvelopeDetails01TurnModerationMetadata55>),
    #[serde(rename = "model/safetyBuffering/updated")]
    ModelSafetyBufferingUpdated(Box<ServerNotificationEnvelopeDetails01ModelSafetyBufferingUpdated56>),
    #[serde(rename = "warning")]
    Warning(Box<ServerNotificationEnvelopeDetails01Warning57>),
    #[serde(rename = "guardianWarning")]
    GuardianWarning(Box<ServerNotificationEnvelopeDetails01GuardianWarning58>),
    #[serde(rename = "deprecationNotice")]
    DeprecationNotice(Box<ServerNotificationEnvelopeDetails01DeprecationNotice59>),
    #[serde(rename = "configWarning")]
    ConfigWarning(Box<ServerNotificationEnvelopeDetails01ConfigWarning60>),
    #[serde(rename = "fuzzyFileSearch/sessionUpdated")]
    FuzzyFileSearchSessionUpdated(Box<ServerNotificationEnvelopeDetails01FuzzyFileSearchSessionUpdated61>),
    #[serde(rename = "fuzzyFileSearch/sessionCompleted")]
    FuzzyFileSearchSessionCompleted(Box<ServerNotificationEnvelopeDetails01FuzzyFileSearchSessionCompleted62>),
    #[serde(rename = "thread/realtime/started")]
    ThreadRealtimeStarted(Box<ServerNotificationEnvelopeDetails01ThreadRealtimeStarted63>),
    #[serde(rename = "thread/realtime/itemAdded")]
    ThreadRealtimeItemAdded(Box<ServerNotificationEnvelopeDetails01ThreadRealtimeItemAdded64>),
    #[serde(rename = "thread/realtime/transcript/delta")]
    ThreadRealtimeTranscriptDelta(Box<ServerNotificationEnvelopeDetails01ThreadRealtimeTranscriptDelta65>),
    #[serde(rename = "thread/realtime/transcript/done")]
    ThreadRealtimeTranscriptDone(Box<ServerNotificationEnvelopeDetails01ThreadRealtimeTranscriptDone66>),
    #[serde(rename = "thread/realtime/outputAudio/delta")]
    ThreadRealtimeOutputAudioDelta(Box<ServerNotificationEnvelopeDetails01ThreadRealtimeOutputAudioDelta67>),
    #[serde(rename = "thread/realtime/sdp")]
    ThreadRealtimeSdp(Box<ServerNotificationEnvelopeDetails01ThreadRealtimeSdp68>),
    #[serde(rename = "thread/realtime/error")]
    ThreadRealtimeError(Box<ServerNotificationEnvelopeDetails01ThreadRealtimeError69>),
    #[serde(rename = "thread/realtime/closed")]
    ThreadRealtimeClosed(Box<ServerNotificationEnvelopeDetails01ThreadRealtimeClosed70>),
    #[serde(rename = "windows/worldWritableWarning")]
    WindowsWorldWritableWarning(Box<ServerNotificationEnvelopeDetails01WindowsWorldWritableWarning71>),
    #[serde(rename = "windowsSandbox/setupCompleted")]
    WindowsSandboxSetupCompleted(Box<ServerNotificationEnvelopeDetails01WindowsSandboxSetupCompleted72>),
    #[serde(rename = "account/login/completed")]
    AccountLoginCompleted(Box<ServerNotificationEnvelopeDetails01AccountLoginCompleted73>),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerNotificationEnvelope {
    #[serde(rename = "emittedAtMs", default, skip_serializing_if = "Option::is_none")]
    pub emitted_at_ms: Option<f64>,
    #[serde(flatten)]
    pub details: ServerNotificationEnvelopeDetails01,
}
