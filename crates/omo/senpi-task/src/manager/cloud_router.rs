use serde_json::Value;

use crate::manager::execution_mode::ExecutionMode;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CloudOffloadMode {
    #[default]
    Auto,
    Always,
    Off,
}

impl CloudOffloadMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "always" => Some(Self::Always),
            "off" => Some(Self::Off),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudOffloadConfig {
    pub mode: CloudOffloadMode,
    pub local_threshold: usize,
    pub offload_isolated: bool,
    pub fallback_to_local: bool,
    pub sync_workspace: bool,
    pub apply_patch: bool,
    pub delete_on_release: bool,
    pub provider: Option<String>,
    pub ttl_seconds: Option<u64>,
    pub ferryx_cli: Option<String>,
}

impl Default for CloudOffloadConfig {
    fn default() -> Self {
        Self {
            mode: CloudOffloadMode::Auto,
            local_threshold: 2,
            offload_isolated: true,
            fallback_to_local: true,
            sync_workspace: true,
            apply_patch: true,
            delete_on_release: false,
            provider: None,
            ttl_seconds: Some(1_800),
            ferryx_cli: None,
        }
    }
}

impl CloudOffloadConfig {
    pub fn from_config(task_settings: &Value) -> Self {
        let mut cfg = Self::default();
        if let Ok(env_mode) = std::env::var("MAHO_CLOUD_OFFLOAD") {
            if let Some(parsed) = CloudOffloadMode::parse(env_mode.trim()) {
                cfg.mode = parsed;
            }
        }
        let Some(cloud) = task_settings.get("cloud").and_then(Value::as_object) else {
            return cfg;
        };
        if let Some(mode) = cloud
            .get("mode")
            .and_then(Value::as_str)
            .and_then(CloudOffloadMode::parse)
        {
            cfg.mode = mode;
        }
        if let Some(n) = cloud
            .get("local_threshold")
            .and_then(Value::as_u64)
            .and_then(|v| usize::try_from(v).ok())
        {
            cfg.local_threshold = n;
        }
        if let Some(v) = cloud.get("offload_isolated").and_then(Value::as_bool) {
            cfg.offload_isolated = v;
        }
        if let Some(v) = cloud.get("fallback_to_local").and_then(Value::as_bool) {
            cfg.fallback_to_local = v;
        }
        if let Some(v) = cloud.get("sync_workspace").and_then(Value::as_bool) {
            cfg.sync_workspace = v;
        }
        if let Some(v) = cloud.get("apply_patch").and_then(Value::as_bool) {
            cfg.apply_patch = v;
        }
        if let Some(v) = cloud.get("delete_on_release").and_then(Value::as_bool) {
            cfg.delete_on_release = v;
        }
        if let Some(v) = cloud
            .get("provider")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "auto")
        {
            cfg.provider = Some(v.to_string());
        }
        if let Some(v) = cloud.get("ttl_seconds").and_then(Value::as_u64) {
            cfg.ttl_seconds = Some(v);
        }
        if let Some(v) = cloud
            .get("ferryx_cli")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            cfg.ferryx_cli = Some(v.to_string());
        }
        cfg
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudRouteDecision {
    Local,
    Cloud { reason: &'static str },
}

pub struct CloudRouteInput<'a> {
    pub config: &'a CloudOffloadConfig,
    pub cloud_available: bool,
    pub base_mode: ExecutionMode,
    pub explicit_spec_mode: Option<ExecutionMode>,
    pub active_children: usize,
    pub run_in_background: bool,
    pub isolated: bool,
    pub has_member_scoped_tools: bool,
    pub has_host_only_extensions: bool,
}

pub fn decide_cloud_offload(input: &CloudRouteInput<'_>) -> CloudRouteDecision {
    if !input.cloud_available || input.config.mode == CloudOffloadMode::Off {
        return CloudRouteDecision::Local;
    }
    if input.has_member_scoped_tools || input.has_host_only_extensions {
        return CloudRouteDecision::Local;
    }
    if matches!(
        input.explicit_spec_mode,
        Some(ExecutionMode::InProcess | ExecutionMode::Process)
    ) {
        return CloudRouteDecision::Local;
    }
    if input.base_mode == ExecutionMode::Cloud || input.explicit_spec_mode == Some(ExecutionMode::Cloud) {
        return CloudRouteDecision::Cloud { reason: "explicit_cloud" };
    }
    if input.config.mode == CloudOffloadMode::Always {
        return CloudRouteDecision::Cloud { reason: "always_cloud" };
    }
    if input.active_children >= input.config.local_threshold {
        return CloudRouteDecision::Cloud { reason: "concurrent_overflow" };
    }
    if input.isolated && input.config.offload_isolated {
        return CloudRouteDecision::Cloud { reason: "isolated_offload" };
    }
    if input.run_in_background && input.active_children >= 1 {
        return CloudRouteDecision::Cloud { reason: "background_fanout" };
    }
    CloudRouteDecision::Local
}

pub fn has_host_only_extensions(extensions: Option<&[String]>) -> bool {
    extensions.is_some_and(|exts| {
        exts.iter()
            .any(|ext| ext == "builtin:task-member" || ext.contains("omo-member"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_input<'a>(config: &'a CloudOffloadConfig) -> CloudRouteInput<'a> {
        CloudRouteInput {
            config,
            cloud_available: true,
            base_mode: ExecutionMode::InProcess,
            explicit_spec_mode: None,
            active_children: 0,
            run_in_background: false,
            isolated: false,
            has_member_scoped_tools: false,
            has_host_only_extensions: false,
        }
    }

    #[test]
    fn single_foreground_task_stays_local_under_auto() {
        let cfg = CloudOffloadConfig::default();
        assert_eq!(decide_cloud_offload(&base_input(&cfg)), CloudRouteDecision::Local);
    }

    #[test]
    fn concurrent_overflow_background_fanout_and_isolated_auto_offload() {
        let cfg = CloudOffloadConfig::default();

        let mut overflow = base_input(&cfg);
        overflow.active_children = 2;
        assert_eq!(
            decide_cloud_offload(&overflow),
            CloudRouteDecision::Cloud { reason: "concurrent_overflow" }
        );

        let mut bg = base_input(&cfg);
        bg.run_in_background = true;
        bg.active_children = 1;
        assert_eq!(
            decide_cloud_offload(&bg),
            CloudRouteDecision::Cloud { reason: "background_fanout" }
        );

        let mut iso = base_input(&cfg);
        iso.isolated = true;
        assert_eq!(
            decide_cloud_offload(&iso),
            CloudRouteDecision::Cloud { reason: "isolated_offload" }
        );
    }

    #[test]
    fn host_bound_children_and_unavailable_cloud_stay_local() {
        let cfg = CloudOffloadConfig {
            mode: CloudOffloadMode::Always,
            ..CloudOffloadConfig::default()
        };

        let mut scoped = base_input(&cfg);
        scoped.has_member_scoped_tools = true;
        assert_eq!(decide_cloud_offload(&scoped), CloudRouteDecision::Local);

        let mut member = base_input(&cfg);
        member.has_host_only_extensions = true;
        assert_eq!(decide_cloud_offload(&member), CloudRouteDecision::Local);

        let mut unavail = base_input(&cfg);
        unavail.cloud_available = false;
        assert_eq!(decide_cloud_offload(&unavail), CloudRouteDecision::Local);
    }
}
