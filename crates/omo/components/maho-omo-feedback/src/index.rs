//! Port of `feedback/index.ts` (+ `runtime-loader.ts`): the `feedback` component with `/feedback`,
//! `/bug` and the `omo.feedback.*` RPCs.
//!
//! Upstream imports the core lazily from a sidecar bundle; natively it is linked in, so no loader
//! runs and [`feedback_runtime_load_failure`] only keeps the user-facing wording.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use maho_ext_api::{
    EventKind, EventResult, Extension, ExtensionApi, ExtensionContext, ExtensionUi,
    ExtensionUiDialogOptions, JsonValue, NotificationType,
    UiFuture,
};
use maho_omo_agent_home::resolve_agent_home;
use serde_json::json;

use crate::draft::{FeedbackKind, UploadRedactor};
use crate::runtime::{
    FeedbackCommandUi, FeedbackCore, FeedbackCoreConfig, FeedbackSession, run_feedback_command,
};
use crate::submit::{FeedbackSubmitRefusal, ReportTransport, SubmitResult, default_transport};

pub const FEEDBACK_COMPONENT_NAME: &str = "feedback";
pub const FEEDBACK_PREPARE_RPC_METHOD: &str = "omo.feedback.prepare";
pub const FEEDBACK_SUBMIT_RPC_METHOD: &str = "omo.feedback.submit";
pub const DEFAULT_API_ORIGIN: &str = "https://api.omo.dev";
pub const FEEDBACK_RUNTIME_ARTIFACT: &str = "extensions/omo-feedback.js";
pub const FEEDBACK_UNAVAILABLE: &str = "Feedback needs an OmO runtime that can remove secrets before upload. Update OmO and try again.";

/// Upstream's loader-failure message for a missing/broken feedback runtime payload.
pub fn feedback_runtime_load_failure(cause: &str) -> String {
    format!(
        "Feedback is unavailable: the feedback runtime ({FEEDBACK_RUNTIME_ARTIFACT}) failed to load: {cause}. This build's plugin payload is incomplete; reinstall omo, then start a new session."
    )
}

#[derive(Clone, Default)]
pub struct FeedbackComponentOptions {
    pub env: Option<BTreeMap<String, String>>,
    pub transport: Option<ReportTransport>,
    pub state_dir: Option<PathBuf>,
    pub omo_version: Option<String>,
    /// Upstream `pi.hostVersion`.
    pub host_version: Option<String>,
    pub channel: Option<String>,
    pub now: Option<Arc<dyn Fn() -> chrono::DateTime<chrono::Utc> + Send + Sync>>,
    /// The host's `redactForUpload`, already summed to a [`crate::draft::Redaction`]. The host API has no
    /// such hook yet, so the wiring passes it here; without it nothing is ever uploaded.
    pub redact_for_upload: Option<UploadRedactor>,
    pub home_dir: Option<PathBuf>,
}

#[derive(Clone, Default)]
pub struct FeedbackComponent {
    pub options: FeedbackComponentOptions,
}

pub fn create_feedback_component(options: FeedbackComponentOptions) -> FeedbackComponent {
    FeedbackComponent { options }
}

impl FeedbackComponent {
    pub const NAME: &'static str = FEEDBACK_COMPONENT_NAME;

    pub fn new(options: FeedbackComponentOptions) -> Self {
        Self { options }
    }
}

#[derive(Default)]
struct LiveSessions {
    by_id: Vec<(String, ExtensionContext)>,
    latest: Option<String>,
}

impl LiveSessions {
    fn get(&self, id: &str) -> Option<&ExtensionContext> {
        self.by_id
            .iter()
            .find(|(key, _)| key == id)
            .map(|(_, ctx)| ctx)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn session_id_of(ctx: &ExtensionContext) -> Option<String> {
    let id = ctx.session_manager.session_id();
    (!id.is_empty()).then(|| id.to_string())
}

fn session_view(ctx: &ExtensionContext) -> FeedbackSession {
    let model = ctx.model.as_ref().map(|model| {
        if model.provider.is_empty() {
            model.id.clone()
        } else {
            format!("{}/{}", model.provider, model.id)
        }
    });
    FeedbackSession {
        cwd: Some(ctx.cwd.to_string_lossy().into_owned()),
        session_id: session_id_of(ctx),
        model,
        entries: ctx
            .session_manager
            .get_branch()
            .into_iter()
            .map(|entry| entry.data)
            .collect(),
    }
}

struct UiAdapter(Arc<dyn ExtensionUi>);

impl FeedbackCommandUi for UiAdapter {
    fn input<'a>(&'a self, title: &'a str, placeholder: &'a str) -> UiFuture<'a, Option<String>> {
        self.0.input(
            title,
            Some(placeholder),
            ExtensionUiDialogOptions::default(),
        )
    }
    fn select<'a>(&'a self, title: &'a str, options: &'a [String]) -> UiFuture<'a, Option<String>> {
        self.0
            .select(title, options, ExtensionUiDialogOptions::default())
    }
    fn notify(&self, message: &str, kind: NotificationType) {
        self.0.notify(message, kind);
    }
}

fn process_env() -> BTreeMap<String, String> {
    std::env::vars().collect()
}

fn state_dir_of(env: &BTreeMap<String, String>, home: &Path) -> PathBuf {
    resolve_agent_home(
        env,
        home,
        &std::env::current_dir().unwrap_or_default(),
        Path::exists,
    )
    .join("omo-senpi")
}

impl Extension for FeedbackComponent {
    fn register(&self, api: &mut ExtensionApi) {
        let options = &self.options;
        let env = options.env.clone().unwrap_or_else(process_env);
        let home = options
            .home_dir
            .clone()
            .or_else(|| {
                env.get("HOME")
                    .filter(|home| !home.is_empty())
                    .map(PathBuf::from)
            })
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        let omo_version = options.omo_version.clone().unwrap_or_else(|| {
            maho_omo_telemetry::product_identity::create_omo_native_product_config().package_version
        });
        let maho_version = options
            .host_version
            .clone()
            .unwrap_or_else(|| "unknown".into());
        let channel = options.channel.clone().unwrap_or_else(|| {
            if omo_version.contains('-') {
                "beta"
            } else {
                "latest"
            }
            .into()
        });
        let now = options
            .now
            .clone()
            .unwrap_or_else(|| Arc::new(chrono::Utc::now));
        let fixed_state_dir = options.state_dir.clone();
        let state_env = env.clone();
        let origin_env = env.clone();

        let core = Arc::new(FeedbackCore::new(FeedbackCoreConfig {
            env,
            now,
            omo_version,
            maho_version,
            channel,
            state_dir: Arc::new(move || {
                fixed_state_dir
                    .clone()
                    .unwrap_or_else(|| state_dir_of(&state_env, &home))
            }),
            api_origin: Arc::new(move || {
                origin_env
                    .get("OMO_SERVICE_API_URL")
                    .cloned()
                    .unwrap_or_else(|| DEFAULT_API_ORIGIN.into())
            }),
            transport: options.transport.clone().unwrap_or_else(default_transport),
        }));
        let redactor = options.redact_for_upload.clone();

        let live: Arc<Mutex<LiveSessions>> = Arc::default();
        let on_start = Arc::clone(&live);
        api.on(
            EventKind::SessionStart,
            Arc::new(move |_, ctx| {
                if let Some(id) = session_id_of(ctx) {
                    let mut sessions = lock(&on_start);
                    sessions.by_id.retain(|(key, _)| *key != id);
                    sessions.by_id.push((id.clone(), ctx.clone()));
                    sessions.latest = Some(id);
                }
                Box::pin(async { Ok(EventResult::None) })
            }),
        );
        let on_shutdown = Arc::clone(&live);
        api.on(
            EventKind::SessionShutdown,
            Arc::new(move |_, ctx| {
                if let Some(id) = session_id_of(ctx) {
                    let mut sessions = lock(&on_shutdown);
                    sessions.by_id.retain(|(key, _)| *key != id);
                    if sessions.latest.as_deref() == Some(id.as_str()) {
                        sessions.latest = sessions.by_id.last().map(|(key, _)| key.clone());
                    }
                }
                Box::pin(async { Ok(EventResult::None) })
            }),
        );

        // `sessionId: null` = no session; a string = that live session; omitted = latest live one.
        let session_for_rpc = {
            let live = Arc::clone(&live);
            move |input: &JsonValue| -> Option<FeedbackSession> {
                let sessions = lock(&live);
                let ctx = match input.get("sessionId") {
                    Some(JsonValue::Null) => None,
                    Some(JsonValue::String(id)) => sessions.get(id),
                    _ => sessions.latest.as_deref().and_then(|id| sessions.get(id)),
                };
                ctx.map(session_view)
            }
        };

        let prepare_core = Arc::clone(&core);
        let prepare_redactor = redactor.clone();
        let rpc_prepare = api.rpc_handle(
            FEEDBACK_PREPARE_RPC_METHOD,
            Arc::new(move |input: JsonValue| {
                let session = session_for_rpc(&input);
                let core = Arc::clone(&prepare_core);
                let redactor = prepare_redactor.clone();
                Box::pin(async move {
                    Ok(match redactor {
                        None => json!({"kind": "unavailable", "reason": FEEDBACK_UNAVAILABLE}),
                        Some(redact) => core.prepare(session, &input, &redact).to_json(),
                    })
                })
            }),
        );
        let submit_core = Arc::clone(&core);
        let submit_redactor = redactor.clone();
        let rpc_submit = api.rpc_handle(
            FEEDBACK_SUBMIT_RPC_METHOD,
            Arc::new(move |input: JsonValue| {
                let core = Arc::clone(&submit_core);
                let redactor = submit_redactor.clone();
                Box::pin(async move {
                    Ok(match redactor {
                        None => SubmitResult::Refused(FeedbackSubmitRefusal::Unavailable),
                        Some(redact) => core.submit(&input, &redact).await,
                    }
                    .to_json())
                })
            }),
        );
        for registered in [rpc_prepare, rpc_submit] {
            if let Err(error) = registered {
                std::panic::panic_any(error);
            }
        }

        for (name, kind, description, hint) in [
            (
                "feedback",
                FeedbackKind::Feedback,
                "Send feedback or an idea to the OmO team",
                "[your feedback]",
            ),
            (
                "bug",
                FeedbackKind::Bug,
                "Report a problem to the OmO team",
                "[what went wrong]",
            ),
        ] {
            let command_core = Arc::clone(&core);
            let command_redactor = redactor.clone();
            api.register_command(
                name,
                Some(description.into()),
                Some(hint.into()),
                Arc::new(move |args: &str, ctx: &ExtensionContext| {
                    let core = Arc::clone(&command_core);
                    let redactor = command_redactor.clone();
                    Box::pin(async move {
                        let ui = UiAdapter(Arc::clone(&ctx.ui));
                        let Some(redact) = redactor else {
                            ui.notify(FEEDBACK_UNAVAILABLE, NotificationType::Warning);
                            return Ok(());
                        };
                        let prepare_core = Arc::clone(&core);
                        let prepare_redact = Arc::clone(&redact);
                        run_feedback_command(
                            kind,
                            args,
                            &ui,
                            || async move {
                                Ok(prepare_core.prepare(
                                    Some(session_view(ctx)),
                                    &json!({}),
                                    &prepare_redact,
                                ))
                            },
                            |input| async move { core.submit(&input, &redact).await },
                        )
                        .await;
                        Ok(())
                    })
                }),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_names_and_messages_match_upstream() {
        assert_eq!(FEEDBACK_PREPARE_RPC_METHOD, "omo.feedback.prepare");
        assert_eq!(FEEDBACK_SUBMIT_RPC_METHOD, "omo.feedback.submit");
        assert_eq!(
            feedback_runtime_load_failure("boom"),
            "Feedback is unavailable: the feedback runtime (extensions/omo-feedback.js) failed to load: boom. This build's plugin payload is incomplete; reinstall omo, then start a new session."
        );
    }

    #[test]
    fn state_dir_lives_under_the_agent_home() {
        // An absolute agent dir on this platform (`/agent` is drive-relative on Windows).
        let agent = std::env::temp_dir().join("agent");
        let env = BTreeMap::from([(
            "MAHO_CODING_AGENT_DIR".to_string(),
            agent.to_string_lossy().into_owned(),
        )]);
        assert_eq!(
            state_dir_of(&env, Path::new("/home/x")),
            agent.join("omo-senpi")
        );
    }
}
