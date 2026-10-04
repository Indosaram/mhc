#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestInitialize1 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::initialize_params::InitializeParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestThreadStart2 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_start_params::ThreadStartParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestThreadResume3 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_resume_params::ThreadResumeParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestThreadFork4 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_fork_params::ThreadForkParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestThreadArchive5 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_archive_params::ThreadArchiveParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestThreadDelete6 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_delete_params::ThreadDeleteParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestThreadUnsubscribe7 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_unsubscribe_params::ThreadUnsubscribeParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestThreadNameSet8 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_set_name_params::ThreadSetNameParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestThreadGoalSet9 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_goal_set_params::ThreadGoalSetParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestThreadGoalGet10 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_goal_get_params::ThreadGoalGetParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestThreadGoalClear11 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_goal_clear_params::ThreadGoalClearParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestThreadMetadataUpdate12 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_metadata_update_params::ThreadMetadataUpdateParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestThreadUnarchive13 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_unarchive_params::ThreadUnarchiveParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestThreadCompactStart14 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_compact_start_params::ThreadCompactStartParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestThreadShellCommand15 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_shell_command_params::ThreadShellCommandParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestThreadApproveGuardianDeniedAction16 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_approve_guardian_denied_action_params::ThreadApproveGuardianDeniedActionParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestThreadRollback17 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_rollback_params::ThreadRollbackParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestThreadList18 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_list_params::ThreadListParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestThreadLoadedList19 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_loaded_list_params::ThreadLoadedListParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestThreadRead20 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_read_params::ThreadReadParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestThreadInjectItems21 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::thread_inject_items_params::ThreadInjectItemsParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestSkillsList22 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::skills_list_params::SkillsListParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestSkillsExtraRootsSet23 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::skills_extra_roots_set_params::SkillsExtraRootsSetParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestHooksList24 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::hooks_list_params::HooksListParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestMarketplaceAdd25 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::marketplace_add_params::MarketplaceAddParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestMarketplaceRemove26 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::marketplace_remove_params::MarketplaceRemoveParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestMarketplaceUpgrade27 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::marketplace_upgrade_params::MarketplaceUpgradeParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestPluginList28 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::plugin_list_params::PluginListParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestPluginInstalled29 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::plugin_installed_params::PluginInstalledParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestPluginRead30 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::plugin_read_params::PluginReadParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestPluginSkillRead31 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::plugin_skill_read_params::PluginSkillReadParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestPluginShareSave32 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::plugin_share_save_params::PluginShareSaveParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestPluginShareUpdateTargets33 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::plugin_share_update_targets_params::PluginShareUpdateTargetsParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestPluginShareList34 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::plugin_share_list_params::PluginShareListParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestPluginShareCheckout35 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::plugin_share_checkout_params::PluginShareCheckoutParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestPluginShareDelete36 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::plugin_share_delete_params::PluginShareDeleteParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestAppRead37 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::apps_read_params::AppsReadParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestAppList38 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::apps_list_params::AppsListParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestAppInstalled39 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::apps_installed_params::AppsInstalledParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestFsReadFile40 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::fs_read_file_params::FsReadFileParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestFsWriteFile41 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::fs_write_file_params::FsWriteFileParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestFsCreateDirectory42 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::fs_create_directory_params::FsCreateDirectoryParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestFsGetMetadata43 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::fs_get_metadata_params::FsGetMetadataParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestFsReadDirectory44 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::fs_read_directory_params::FsReadDirectoryParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestFsRemove45 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::fs_remove_params::FsRemoveParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestFsCopy46 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::fs_copy_params::FsCopyParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestFsWatch47 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::fs_watch_params::FsWatchParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestFsUnwatch48 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::fs_unwatch_params::FsUnwatchParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestSkillsConfigWrite49 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::skills_config_write_params::SkillsConfigWriteParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestPluginInstall50 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::plugin_install_params::PluginInstallParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestPluginUninstall51 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::plugin_uninstall_params::PluginUninstallParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestTurnStart52 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::turn_start_params::TurnStartParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestTurnSteer53 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::turn_steer_params::TurnSteerParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestTurnInterrupt54 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::turn_interrupt_params::TurnInterruptParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestReviewStart55 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::review_start_params::ReviewStartParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestModelList56 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::model_list_params::ModelListParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestModelProviderCapabilitiesRead57 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::model_provider_capabilities_read_params::ModelProviderCapabilitiesReadParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestExperimentalFeatureList58 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::experimental_feature_list_params::ExperimentalFeatureListParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestPermissionProfileList59 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::permission_profile_list_params::PermissionProfileListParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestExperimentalFeatureEnablementSet60 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::experimental_feature_enablement_set_params::ExperimentalFeatureEnablementSetParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestMcpServerOauthLogin61 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::mcp_server_oauth_login_params::McpServerOauthLoginParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestConfigMcpServerReload62 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: (),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestMcpServerStatusList63 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::list_mcp_server_status_params::ListMcpServerStatusParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestMcpServerResourceRead64 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::mcp_resource_read_params::McpResourceReadParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestMcpServerToolCall65 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::mcp_server_tool_call_params::McpServerToolCallParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestWindowsSandboxSetupStart66 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::windows_sandbox_setup_start_params::WindowsSandboxSetupStartParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestWindowsSandboxReadiness67 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: (),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestAccountLoginStart68 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::login_account_params::LoginAccountParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestAccountLoginCancel69 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::cancel_login_account_params::CancelLoginAccountParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestAccountLogout70 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: (),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestAccountRateLimitsRead71 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: (),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestAccountRateLimitResetCreditConsume72 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::consume_account_rate_limit_reset_credit_params::ConsumeAccountRateLimitResetCreditParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestAccountUsageRead73 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: (),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestAccountWorkspaceMessagesRead74 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: (),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestAccountSendAddCreditsNudgeEmail75 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::send_add_credits_nudge_email_params::SendAddCreditsNudgeEmailParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestFeedbackUpload76 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::feedback_upload_params::FeedbackUploadParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestCommandExec77 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::command_exec_params::CommandExecParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestCommandExecWrite78 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::command_exec_write_params::CommandExecWriteParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestCommandExecTerminate79 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::command_exec_terminate_params::CommandExecTerminateParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestCommandExecResize80 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::command_exec_resize_params::CommandExecResizeParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestConfigRead81 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::config_read_params::ConfigReadParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestExternalAgentConfigDetect82 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::external_agent_config_detect_params::ExternalAgentConfigDetectParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestExternalAgentConfigImport83 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::external_agent_config_import_params::ExternalAgentConfigImportParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestExternalAgentConfigImportReadHistories84 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: (),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestConfigValueWrite85 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::config_value_write_params::ConfigValueWriteParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestConfigBatchWrite86 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::config_batch_write_params::ConfigBatchWriteParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestConfigRequirementsRead87 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: (),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestAccountRead88 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::get_account_params::GetAccountParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestGetConversationSummary89 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::get_conversation_summary_params::GetConversationSummaryParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestGitDiffToRemote90 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::git_diff_to_remote_params::GitDiffToRemoteParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestGetAuthStatus91 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::get_auth_status_params::GetAuthStatusParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientRequestFuzzyFileSearch92 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::fuzzy_file_search_params::FuzzyFileSearchParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "method")]
pub enum ClientRequest {
    #[serde(rename = "initialize")]
    Initialize(Box<ClientRequestInitialize1>),
    #[serde(rename = "thread/start")]
    ThreadStart(Box<ClientRequestThreadStart2>),
    #[serde(rename = "thread/resume")]
    ThreadResume(Box<ClientRequestThreadResume3>),
    #[serde(rename = "thread/fork")]
    ThreadFork(Box<ClientRequestThreadFork4>),
    #[serde(rename = "thread/archive")]
    ThreadArchive(Box<ClientRequestThreadArchive5>),
    #[serde(rename = "thread/delete")]
    ThreadDelete(Box<ClientRequestThreadDelete6>),
    #[serde(rename = "thread/unsubscribe")]
    ThreadUnsubscribe(Box<ClientRequestThreadUnsubscribe7>),
    #[serde(rename = "thread/name/set")]
    ThreadNameSet(Box<ClientRequestThreadNameSet8>),
    #[serde(rename = "thread/goal/set")]
    ThreadGoalSet(Box<ClientRequestThreadGoalSet9>),
    #[serde(rename = "thread/goal/get")]
    ThreadGoalGet(Box<ClientRequestThreadGoalGet10>),
    #[serde(rename = "thread/goal/clear")]
    ThreadGoalClear(Box<ClientRequestThreadGoalClear11>),
    #[serde(rename = "thread/metadata/update")]
    ThreadMetadataUpdate(Box<ClientRequestThreadMetadataUpdate12>),
    #[serde(rename = "thread/unarchive")]
    ThreadUnarchive(Box<ClientRequestThreadUnarchive13>),
    #[serde(rename = "thread/compact/start")]
    ThreadCompactStart(Box<ClientRequestThreadCompactStart14>),
    #[serde(rename = "thread/shellCommand")]
    ThreadShellCommand(Box<ClientRequestThreadShellCommand15>),
    #[serde(rename = "thread/approveGuardianDeniedAction")]
    ThreadApproveGuardianDeniedAction(Box<ClientRequestThreadApproveGuardianDeniedAction16>),
    #[serde(rename = "thread/rollback")]
    ThreadRollback(Box<ClientRequestThreadRollback17>),
    #[serde(rename = "thread/list")]
    ThreadList(Box<ClientRequestThreadList18>),
    #[serde(rename = "thread/loaded/list")]
    ThreadLoadedList(Box<ClientRequestThreadLoadedList19>),
    #[serde(rename = "thread/read")]
    ThreadRead(Box<ClientRequestThreadRead20>),
    #[serde(rename = "thread/inject_items")]
    ThreadInjectItems(Box<ClientRequestThreadInjectItems21>),
    #[serde(rename = "skills/list")]
    SkillsList(Box<ClientRequestSkillsList22>),
    #[serde(rename = "skills/extraRoots/set")]
    SkillsExtraRootsSet(Box<ClientRequestSkillsExtraRootsSet23>),
    #[serde(rename = "hooks/list")]
    HooksList(Box<ClientRequestHooksList24>),
    #[serde(rename = "marketplace/add")]
    MarketplaceAdd(Box<ClientRequestMarketplaceAdd25>),
    #[serde(rename = "marketplace/remove")]
    MarketplaceRemove(Box<ClientRequestMarketplaceRemove26>),
    #[serde(rename = "marketplace/upgrade")]
    MarketplaceUpgrade(Box<ClientRequestMarketplaceUpgrade27>),
    #[serde(rename = "plugin/list")]
    PluginList(Box<ClientRequestPluginList28>),
    #[serde(rename = "plugin/installed")]
    PluginInstalled(Box<ClientRequestPluginInstalled29>),
    #[serde(rename = "plugin/read")]
    PluginRead(Box<ClientRequestPluginRead30>),
    #[serde(rename = "plugin/skill/read")]
    PluginSkillRead(Box<ClientRequestPluginSkillRead31>),
    #[serde(rename = "plugin/share/save")]
    PluginShareSave(Box<ClientRequestPluginShareSave32>),
    #[serde(rename = "plugin/share/updateTargets")]
    PluginShareUpdateTargets(Box<ClientRequestPluginShareUpdateTargets33>),
    #[serde(rename = "plugin/share/list")]
    PluginShareList(Box<ClientRequestPluginShareList34>),
    #[serde(rename = "plugin/share/checkout")]
    PluginShareCheckout(Box<ClientRequestPluginShareCheckout35>),
    #[serde(rename = "plugin/share/delete")]
    PluginShareDelete(Box<ClientRequestPluginShareDelete36>),
    #[serde(rename = "app/read")]
    AppRead(Box<ClientRequestAppRead37>),
    #[serde(rename = "app/list")]
    AppList(Box<ClientRequestAppList38>),
    #[serde(rename = "app/installed")]
    AppInstalled(Box<ClientRequestAppInstalled39>),
    #[serde(rename = "fs/readFile")]
    FsReadFile(Box<ClientRequestFsReadFile40>),
    #[serde(rename = "fs/writeFile")]
    FsWriteFile(Box<ClientRequestFsWriteFile41>),
    #[serde(rename = "fs/createDirectory")]
    FsCreateDirectory(Box<ClientRequestFsCreateDirectory42>),
    #[serde(rename = "fs/getMetadata")]
    FsGetMetadata(Box<ClientRequestFsGetMetadata43>),
    #[serde(rename = "fs/readDirectory")]
    FsReadDirectory(Box<ClientRequestFsReadDirectory44>),
    #[serde(rename = "fs/remove")]
    FsRemove(Box<ClientRequestFsRemove45>),
    #[serde(rename = "fs/copy")]
    FsCopy(Box<ClientRequestFsCopy46>),
    #[serde(rename = "fs/watch")]
    FsWatch(Box<ClientRequestFsWatch47>),
    #[serde(rename = "fs/unwatch")]
    FsUnwatch(Box<ClientRequestFsUnwatch48>),
    #[serde(rename = "skills/config/write")]
    SkillsConfigWrite(Box<ClientRequestSkillsConfigWrite49>),
    #[serde(rename = "plugin/install")]
    PluginInstall(Box<ClientRequestPluginInstall50>),
    #[serde(rename = "plugin/uninstall")]
    PluginUninstall(Box<ClientRequestPluginUninstall51>),
    #[serde(rename = "turn/start")]
    TurnStart(Box<ClientRequestTurnStart52>),
    #[serde(rename = "turn/steer")]
    TurnSteer(Box<ClientRequestTurnSteer53>),
    #[serde(rename = "turn/interrupt")]
    TurnInterrupt(Box<ClientRequestTurnInterrupt54>),
    #[serde(rename = "review/start")]
    ReviewStart(Box<ClientRequestReviewStart55>),
    #[serde(rename = "model/list")]
    ModelList(Box<ClientRequestModelList56>),
    #[serde(rename = "modelProvider/capabilities/read")]
    ModelProviderCapabilitiesRead(Box<ClientRequestModelProviderCapabilitiesRead57>),
    #[serde(rename = "experimentalFeature/list")]
    ExperimentalFeatureList(Box<ClientRequestExperimentalFeatureList58>),
    #[serde(rename = "permissionProfile/list")]
    PermissionProfileList(Box<ClientRequestPermissionProfileList59>),
    #[serde(rename = "experimentalFeature/enablement/set")]
    ExperimentalFeatureEnablementSet(Box<ClientRequestExperimentalFeatureEnablementSet60>),
    #[serde(rename = "mcpServer/oauth/login")]
    McpServerOauthLogin(Box<ClientRequestMcpServerOauthLogin61>),
    #[serde(rename = "config/mcpServer/reload")]
    ConfigMcpServerReload(Box<ClientRequestConfigMcpServerReload62>),
    #[serde(rename = "mcpServerStatus/list")]
    McpServerStatusList(Box<ClientRequestMcpServerStatusList63>),
    #[serde(rename = "mcpServer/resource/read")]
    McpServerResourceRead(Box<ClientRequestMcpServerResourceRead64>),
    #[serde(rename = "mcpServer/tool/call")]
    McpServerToolCall(Box<ClientRequestMcpServerToolCall65>),
    #[serde(rename = "windowsSandbox/setupStart")]
    WindowsSandboxSetupStart(Box<ClientRequestWindowsSandboxSetupStart66>),
    #[serde(rename = "windowsSandbox/readiness")]
    WindowsSandboxReadiness(Box<ClientRequestWindowsSandboxReadiness67>),
    #[serde(rename = "account/login/start")]
    AccountLoginStart(Box<ClientRequestAccountLoginStart68>),
    #[serde(rename = "account/login/cancel")]
    AccountLoginCancel(Box<ClientRequestAccountLoginCancel69>),
    #[serde(rename = "account/logout")]
    AccountLogout(Box<ClientRequestAccountLogout70>),
    #[serde(rename = "account/rateLimits/read")]
    AccountRateLimitsRead(Box<ClientRequestAccountRateLimitsRead71>),
    #[serde(rename = "account/rateLimitResetCredit/consume")]
    AccountRateLimitResetCreditConsume(Box<ClientRequestAccountRateLimitResetCreditConsume72>),
    #[serde(rename = "account/usage/read")]
    AccountUsageRead(Box<ClientRequestAccountUsageRead73>),
    #[serde(rename = "account/workspaceMessages/read")]
    AccountWorkspaceMessagesRead(Box<ClientRequestAccountWorkspaceMessagesRead74>),
    #[serde(rename = "account/sendAddCreditsNudgeEmail")]
    AccountSendAddCreditsNudgeEmail(Box<ClientRequestAccountSendAddCreditsNudgeEmail75>),
    #[serde(rename = "feedback/upload")]
    FeedbackUpload(Box<ClientRequestFeedbackUpload76>),
    #[serde(rename = "command/exec")]
    CommandExec(Box<ClientRequestCommandExec77>),
    #[serde(rename = "command/exec/write")]
    CommandExecWrite(Box<ClientRequestCommandExecWrite78>),
    #[serde(rename = "command/exec/terminate")]
    CommandExecTerminate(Box<ClientRequestCommandExecTerminate79>),
    #[serde(rename = "command/exec/resize")]
    CommandExecResize(Box<ClientRequestCommandExecResize80>),
    #[serde(rename = "config/read")]
    ConfigRead(Box<ClientRequestConfigRead81>),
    #[serde(rename = "externalAgentConfig/detect")]
    ExternalAgentConfigDetect(Box<ClientRequestExternalAgentConfigDetect82>),
    #[serde(rename = "externalAgentConfig/import")]
    ExternalAgentConfigImport(Box<ClientRequestExternalAgentConfigImport83>),
    #[serde(rename = "externalAgentConfig/import/readHistories")]
    ExternalAgentConfigImportReadHistories(Box<ClientRequestExternalAgentConfigImportReadHistories84>),
    #[serde(rename = "config/value/write")]
    ConfigValueWrite(Box<ClientRequestConfigValueWrite85>),
    #[serde(rename = "config/batchWrite")]
    ConfigBatchWrite(Box<ClientRequestConfigBatchWrite86>),
    #[serde(rename = "configRequirements/read")]
    ConfigRequirementsRead(Box<ClientRequestConfigRequirementsRead87>),
    #[serde(rename = "account/read")]
    AccountRead(Box<ClientRequestAccountRead88>),
    #[serde(rename = "getConversationSummary")]
    GetConversationSummary(Box<ClientRequestGetConversationSummary89>),
    #[serde(rename = "gitDiffToRemote")]
    GitDiffToRemote(Box<ClientRequestGitDiffToRemote90>),
    #[serde(rename = "getAuthStatus")]
    GetAuthStatus(Box<ClientRequestGetAuthStatus91>),
    #[serde(rename = "fuzzyFileSearch")]
    FuzzyFileSearch(Box<ClientRequestFuzzyFileSearch92>),
}
