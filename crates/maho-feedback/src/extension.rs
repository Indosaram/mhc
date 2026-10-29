//! The `<builtin:feedback>` extension: `/feedback` and `/bug`, the prepare/submit RPCs, and the
//! live-session tracking the prepare RPC uses to pick a session (spec 2, 5.1, 6).

use crate::flow::{self, ReportsBackend, SessionUi};
use crate::model::ReportKind;
use crate::reports::{
    ConfigLoader, Host, HttpRequest, HttpResponse, LiveSession, PrepareRequest, Redacted, Redactor,
    Reports, Store, Transport, shared_store,
};
use maho_ext_api::{
    EventKind, EventResult, Extension, ExtensionApi, ExtensionContext, ExtensionEvent,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

/// RPC method that builds a draft.
pub const PREPARE_RPC: &str = "omo.feedback.prepare";
/// RPC method that submits a draft.
pub const SUBMIT_RPC: &str = "omo.feedback.submit";

/// The feedback extension.
#[derive(Clone)]
pub struct FeedbackExtension {
    /// Agent directory (logs and state dir); `None` follows the latest session's agent dir.
    pub agent_dir: Option<PathBuf>,
    /// Product version reported in the environment section and report body.
    pub product_version: String,
    /// Draft and live-session store; [`shared_store`] by default.
    pub store: Arc<Store>,
    /// Intake transport; [`http_transport`] by default.
    pub transport: Transport,
}

impl std::fmt::Debug for FeedbackExtension {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeedbackExtension")
            .field("agent_dir", &self.agent_dir)
            .field("product_version", &self.product_version)
            .finish_non_exhaustive()
    }
}

impl Default for FeedbackExtension {
    fn default() -> Self {
        Self {
            agent_dir: None,
            product_version: env!("CARGO_PKG_VERSION").into(),
            store: shared_store(),
            transport: http_transport(),
        }
    }
}

impl FeedbackExtension {
    /// Follows each session's agent dir; reports `product_version`.
    pub fn with_product_version(product_version: impl Into<String>) -> Self {
        Self {
            product_version: product_version.into(),
            ..Self::default()
        }
    }

    pub fn new(agent_dir: impl Into<PathBuf>, product_version: impl Into<String>) -> Self {
        Self {
            agent_dir: Some(agent_dir.into()),
            product_version: product_version.into(),
            ..Self::default()
        }
    }
}

/// POSTs over HTTPS with `reqwest`.
pub fn http_transport() -> Transport {
    Arc::new(|request: HttpRequest| {
        let future: crate::reports::TransportFuture = Box::pin(async move {
            let client = reqwest::Client::new();
            let mut builder = client.post(&request.url).body(request.body);
            for (name, value) in &request.headers {
                builder = builder.header(name.as_str(), value.as_str());
            }
            let response = builder.send().await.map_err(|error| error.to_string())?;
            let status = response.status().as_u16();
            let body = response.text().await.map_err(|error| error.to_string())?;
            Ok(HttpResponse { status, body })
        });
        future
    })
}

/// The merged user configuration for `cwd`.
fn load_config() -> ConfigLoader {
    Arc::new(|cwd: &Path| {
        let env = maho_config::process_env();
        maho_config::load_native(cwd, &env).view
    })
}

/// `provider/id` of a model.
fn model_ref(model: Option<&maho_ext_api::Model>) -> Option<String> {
    model.map(|model| format!("{}/{}", model.provider, model.id))
}

/// The session behind `ctx` as a [`LiveSession`].
fn live_session(ctx: &ExtensionContext) -> LiveSession {
    let manager = Arc::clone(&ctx.session_manager);
    LiveSession {
        session_id: ctx.session_manager.session_id().to_owned(),
        cwd: ctx.cwd.clone(),
        model: model_ref(ctx.model.as_ref()),
        entries: Arc::new(move || manager.get_branch()),
    }
}

struct Shared {
    config: FeedbackExtension,
    api: Arc<ExtensionApi>,
    cwd: PathBuf,
    /// Agent dir seen at the latest session start.
    seen_agent_dir: Mutex<Option<PathBuf>>,
}

impl Shared {
    /// The host redactor, when the host has one.
    fn redactor(&self) -> Option<Redactor> {
        let probe = self.api.redact_for_upload("", &[]);
        if !matches!(probe, Ok(Some(_))) {
            return None;
        }
        let api = Arc::clone(&self.api);
        Some(Arc::new(move |text: &str| {
            match api.redact_for_upload(text, &[]) {
                Ok(Some(out)) => Redacted {
                    text: out.text,
                    count: out.counts.values().sum(),
                },
                // The redactor went away mid-flight: nothing of the text may be kept.
                _ => Redacted {
                    text: String::new(),
                    count: usize::from(!text.is_empty()),
                },
            }
        }))
    }

    fn reports(&self, cwd: Option<&Path>, agent_dir: Option<&Path>) -> Reports {
        let agent_dir = self
            .config
            .agent_dir
            .clone()
            .or_else(|| agent_dir.map(Path::to_path_buf))
            .or_else(|| {
                self.seen_agent_dir
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone()
            })
            .unwrap_or_else(|| self.cwd.clone());
        let host = Host {
            redactor: self.redactor(),
            transport: Arc::clone(&self.config.transport),
            env: Arc::new(|key: &str| std::env::var(key).ok()),
            clock: Arc::new(jiff::Timestamp::now),
            config: load_config(),
            agent_dir,
            default_cwd: cwd.map_or_else(|| self.cwd.clone(), Path::to_path_buf),
            product_version: self.config.product_version.clone(),
            host_version: self
                .api
                .host_version()
                .ok()
                .flatten()
                .unwrap_or_else(|| "unknown".into()),
            host_name: gethostname::gethostname().to_string_lossy().into_owned(),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            kernel: crate::collect::kernel_release(),
        };
        Reports::new(host, Arc::clone(&self.config.store))
    }
}

fn register_command(api: &mut ExtensionApi, shared: &Arc<Shared>, kind: ReportKind) {
    let (name, description, hint) = match kind {
        ReportKind::Bug => (
            "bug",
            "Report a problem: review exactly what will be sent, then send it",
            "[what went wrong]",
        ),
        ReportKind::Feedback | ReportKind::Other => (
            "feedback",
            "Send feedback: review exactly what will be sent, then send it",
            "[your feedback]",
        ),
    };
    let shared = Arc::clone(shared);
    api.register_command(
        name,
        Some(description.into()),
        Some(hint.into()),
        Arc::new(move |args, ctx| {
            let shared = Arc::clone(&shared);
            Box::pin(async move {
                let backend = ReportsBackend {
                    reports: shared.reports(Some(ctx.cwd.as_path()), Some(ctx.agent_dir.as_path())),
                    request: PrepareRequest {
                        session: Some(live_session(ctx)),
                        ..PrepareRequest::default()
                    },
                };
                flow::run(kind, args, &SessionUi(ctx.ui.as_ref()), &backend).await;
                Ok(())
            })
        }),
    );
}

impl Extension for FeedbackExtension {
    fn register(&self, api: &mut ExtensionApi) {
        let shared = Arc::new(Shared {
            config: self.clone(),
            api: Arc::new(ExtensionApi::new(
                api.registered.clone(),
                api.profile.clone(),
                api.events.clone(),
                api.runtime.clone(),
            )),
            cwd: api.cwd.clone(),
            seen_agent_dir: Mutex::new(None),
        });

        register_command(api, &shared, ReportKind::Feedback);
        register_command(api, &shared, ReportKind::Bug);

        let prepare = Arc::clone(&shared);
        if let Err(error) = api.rpc_handle(
            PREPARE_RPC,
            Arc::new(move |input| {
                let shared = Arc::clone(&prepare);
                Box::pin(async move { Ok(shared.reports(None, None).prepare_json(&input)) })
            }),
        ) {
            maho_ext_api::diagnostic(&format!("feedback: {}", error.message));
        }

        let submit = Arc::clone(&shared);
        if let Err(error) = api.rpc_handle(
            SUBMIT_RPC,
            Arc::new(move |input| {
                let shared = Arc::clone(&submit);
                Box::pin(async move { Ok(shared.reports(None, None).submit_json(&input).await) })
            }),
        ) {
            maho_ext_api::diagnostic(&format!("feedback: {}", error.message));
        }

        let started = Arc::clone(&shared);
        api.on(
            EventKind::SessionStart,
            Arc::new(move |_event, ctx| {
                let shared = Arc::clone(&started);
                Box::pin(async move {
                    *shared
                        .seen_agent_dir
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner) = Some(ctx.agent_dir.clone());
                    shared.config.store.session_started(live_session(ctx));
                    Ok(EventResult::None)
                })
            }),
        );

        let ended = Arc::clone(&shared);
        api.on(
            EventKind::SessionShutdown,
            Arc::new(move |_event, ctx| {
                let shared = Arc::clone(&ended);
                Box::pin(async move {
                    shared
                        .config
                        .store
                        .session_ended(ctx.session_manager.session_id());
                    Ok(EventResult::None)
                })
            }),
        );

        let selected = Arc::clone(&shared);
        api.on(
            EventKind::ModelSelect,
            Arc::new(move |event, ctx| {
                let shared = Arc::clone(&selected);
                Box::pin(async move {
                    if let ExtensionEvent::ModelSelect(select) = event {
                        shared.config.store.set_model(
                            ctx.session_manager.session_id(),
                            model_ref(Some(&select.model)),
                        );
                    }
                    Ok(EventResult::None)
                })
            }),
        );
    }
}
