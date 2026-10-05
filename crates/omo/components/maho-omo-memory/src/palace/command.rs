use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use maho_ext_api::{
    ExtensionApi, ExtensionContext, ExtensionFailure, ExtensionUi, NotificationType,
};

use super::PalaceError;
use super::generator::{GeneratePalaceOptions, generate_palace_html};
use super::people::PalacePeopleOptions;
use crate::context::MemoryIdentityContext;

pub const UNBOUND_NOTICE: &str =
    "memory is not bound in this session, so /palace has nothing to render";
pub const REMOTE_ENV_KEYS: [&str; 4] = ["TMUX", "SSH_CONNECTION", "SSH_TTY", "SSH_CLIENT"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PalaceNotificationLevel {
    Info,
    Warning,
}

pub trait PalaceCommandUi: Send + Sync {
    fn notify(&self, message: &str, level: PalaceNotificationLevel);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PalaceExecOutcome {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub type PalaceExecFuture =
    Pin<Box<dyn Future<Output = Result<PalaceExecOutcome, String>> + Send>>;
pub type PalaceExec = Arc<dyn Fn(&str, Vec<String>) -> PalaceExecFuture + Send + Sync>;
pub type PalaceOutput = Arc<dyn Fn(&str) + Send + Sync>;

pub struct PalaceCommandContext {
    pub has_ui: bool,
    pub ui: Option<Arc<dyn PalaceCommandUi>>,
    pub env: BTreeMap<String, String>,
    pub platform: String,
    pub output: Option<PalaceOutput>,
    pub exec: Option<PalaceExec>,
}

pub type PalaceExtensionResolver =
    Arc<dyn Fn(&ExtensionContext) -> Option<MemoryIdentityContext> + Send + Sync>;

pub type PalaceContextResolver =
    Arc<dyn Fn(&PalaceCommandContext) -> Option<MemoryIdentityContext> + Send + Sync>;
pub type PalacePeopleResolver = Arc<dyn Fn() -> Option<PalacePeopleOptions> + Send + Sync>;

impl PalaceCommandContext {
    pub fn from_extension(context: &ExtensionContext) -> Self {
        Self {
            has_ui: context.has_ui,
            ui: Some(Arc::new(ExtensionUiPalace {
                ui: Arc::clone(&context.ui),
            })),
            env: std::env::vars().collect(),
            platform: host_platform(),
            output: Some(Arc::new(|text: &str| println!("{text}"))),
            exec: Some(system_palace_exec()),
        }
    }
}

pub fn register_palace_command(
    api: &mut ExtensionApi,
    resolve: PalaceExtensionResolver,
    resolve_people: Option<PalacePeopleResolver>,
) {
    api.register_command(
        "palace",
        Some("Generate the memory palace HTML viewer for the bound identity.".to_string()),
        None,
        Arc::new(move |_args, context| {
            let resolve_people = resolve_people.clone();
            let palace_context = PalaceCommandContext::from_extension(context);
            let identity_context = resolve(context);
            let resolver: PalaceContextResolver =
                Arc::new(move |_palace: &PalaceCommandContext| identity_context.clone());
            Box::pin(async move {
                run_palace_command(&resolver, &palace_context, resolve_people.as_ref())
                    .await
                    .map_err(|error| ExtensionFailure::new(error.to_string()))
            })
        }),
    );
}

pub async fn run_palace_command(
    resolve: &PalaceContextResolver,
    context: &PalaceCommandContext,
    resolve_people: Option<&PalacePeopleResolver>,
) -> Result<(), PalaceError> {
    let Some(identity_context) = resolve(context) else {
        report(context, UNBOUND_NOTICE, PalaceNotificationLevel::Warning);
        return Ok(());
    };

    let people = resolve_people.and_then(|resolver| resolver());
    let result = generate_palace_html(
        &identity_context,
        GeneratePalaceOptions {
            people,
            ..Default::default()
        },
    )?;
    let path = result.path.display().to_string();

    if !context.has_ui {
        if let Some(output) = &context.output {
            output(&path);
        }
        return Ok(());
    }

    let Some(exec) = &context.exec else {
        report(context, &path, PalaceNotificationLevel::Info);
        return Ok(());
    };
    if is_remote_session(&context.env) {
        report(context, &path, PalaceNotificationLevel::Info);
        return Ok(());
    }

    let (command, mut args) = opener_for(&context.platform);
    args.push(path.clone());
    match exec(&command, args).await {
        Ok(outcome) if outcome.code == 0 => Ok(()),
        _ => {
            report(context, &path, PalaceNotificationLevel::Info);
            Ok(())
        }
    }
}

pub fn is_remote_session(env: &BTreeMap<String, String>) -> bool {
    REMOTE_ENV_KEYS
        .iter()
        .any(|key| env.get(*key).map(|value| !value.is_empty()).unwrap_or(false))
}

fn opener_for(platform: &str) -> (String, Vec<String>) {
    match platform {
        "darwin" => ("open".to_string(), Vec::new()),
        "win32" => ("start".to_string(), vec![String::new()]),
        _ => ("xdg-open".to_string(), Vec::new()),
    }
}

fn report(context: &PalaceCommandContext, message: &str, level: PalaceNotificationLevel) {
    if let Some(ui) = &context.ui {
        ui.notify(message, level);
        return;
    }
    if let Some(output) = &context.output {
        output(message);
    }
}

fn host_platform() -> String {
    match std::env::consts::OS {
        "macos" => "darwin".to_string(),
        "windows" => "win32".to_string(),
        other => other.to_string(),
    }
}

pub fn system_palace_exec() -> PalaceExec {
    Arc::new(|command: &str, args: Vec<String>| -> PalaceExecFuture {
        let command = command.to_string();
        Box::pin(async move {
            let output = tokio::process::Command::new(&command)
                .args(&args)
                .output()
                .await
                .map_err(|error| error.to_string())?;
            Ok(PalaceExecOutcome {
                code: output.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            })
        })
    })
}

struct ExtensionUiPalace {
    ui: Arc<dyn ExtensionUi>,
}

impl PalaceCommandUi for ExtensionUiPalace {
    fn notify(&self, message: &str, level: PalaceNotificationLevel) {
        let kind = match level {
            PalaceNotificationLevel::Info => NotificationType::Info,
            PalaceNotificationLevel::Warning => NotificationType::Warning,
        };
        self.ui.notify(message, kind);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palace::test_support::create_palace_fixture;
    use std::sync::Mutex;

    /// The `(command, args)` pairs the palace opener handed to the platform exec seam.
    type ExecCalls = Arc<Mutex<Vec<(String, Vec<String>)>>>;

    struct FakeUi {
        notifications: Arc<Mutex<Vec<(String, PalaceNotificationLevel)>>>,
    }

    impl PalaceCommandUi for FakeUi {
        fn notify(&self, message: &str, level: PalaceNotificationLevel) {
            self.notifications
                .lock()
                .unwrap()
                .push((message.to_string(), level));
        }
    }

    struct Harness {
        context: PalaceCommandContext,
        exec_calls: ExecCalls,
        notifications: Arc<Mutex<Vec<(String, PalaceNotificationLevel)>>>,
        outputs: Arc<Mutex<Vec<String>>>,
    }

    fn harness(has_ui: bool, platform: &str, env: Vec<(&str, &str)>) -> Harness {
        let exec_calls = Arc::new(Mutex::new(Vec::new()));
        let notifications = Arc::new(Mutex::new(Vec::new()));
        let outputs = Arc::new(Mutex::new(Vec::new()));
        let exec_calls_for_exec = Arc::clone(&exec_calls);
        let outputs_for_output = Arc::clone(&outputs);
        Harness {
            exec_calls,
            notifications: Arc::clone(&notifications),
            outputs,
            context: PalaceCommandContext {
                has_ui,
                ui: Some(Arc::new(FakeUi { notifications })),
                env: env
                    .into_iter()
                    .map(|(key, value)| (key.to_string(), value.to_string()))
                    .collect(),
                platform: platform.to_string(),
                output: Some(Arc::new(move |text: &str| {
                    outputs_for_output.lock().unwrap().push(text.to_string());
                })),
                exec: Some(Arc::new(move |command: &str, args: Vec<String>| -> PalaceExecFuture {
                    exec_calls_for_exec
                        .lock()
                        .unwrap()
                        .push((command.to_string(), args));
                    Box::pin(async {
                        Ok(PalaceExecOutcome {
                            code: 0,
                            stdout: String::new(),
                            stderr: String::new(),
                        })
                    })
                })),
            },
        }
    }

    fn resolver(context: MemoryIdentityContext) -> PalaceContextResolver {
        Arc::new(move |_| Some(context.clone()))
    }

    #[test]
    fn registration_exposes_description_and_handler() {
        let mut api = ExtensionApi::new(
            maho_ext_api::LoadedExtension::new("memory-palace", Default::default(), Default::default()),
            Default::default(),
            Default::default(),
            Default::default(),
        );

        register_palace_command(&mut api, Arc::new(|_: &ExtensionContext| None::<MemoryIdentityContext>), None);

        let registration = api
            .registered
            .commands
            .iter()
            .find(|entry| entry.name == "palace")
            .unwrap();
        assert!(registration.description.is_some());
    }

    #[tokio::test]
    async fn darwin_opens_the_generated_file() {
        let fixture = create_palace_fixture(false);
        let surface = harness(true, "darwin", Vec::new());

        let resolve = resolver(fixture.context.clone());
        run_palace_command(&resolve, &surface.context, None).await.unwrap();

        let calls = surface.exec_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "open");
        assert!(std::path::Path::new(&calls[0].1[0]).exists());
    }

    #[tokio::test]
    async fn linux_and_windows_use_platform_openers() {
        let fixture = create_palace_fixture(false);
        let linux = harness(true, "linux", Vec::new());
        let windows = harness(true, "win32", Vec::new());

        let resolve = resolver(fixture.context.clone());
        run_palace_command(&resolve, &linux.context, None).await.unwrap();
        run_palace_command(&resolve, &windows.context, None).await.unwrap();

        assert_eq!(linux.exec_calls.lock().unwrap()[0].0, "xdg-open");
        assert_eq!(windows.exec_calls.lock().unwrap()[0].0, "start");
    }

    #[tokio::test]
    async fn remote_session_prints_the_path() {
        let fixture = create_palace_fixture(false);
        let tmux = harness(true, "darwin", vec![("TMUX", "/tmp/tmux-1000/default,1,0")]);
        let ssh = harness(true, "darwin", vec![("SSH_CONNECTION", "10.0.0.2 22 10.0.0.1 22")]);

        let resolve = resolver(fixture.context.clone());
        run_palace_command(&resolve, &tmux.context, None).await.unwrap();
        run_palace_command(&resolve, &ssh.context, None).await.unwrap();

        assert!(tmux.exec_calls.lock().unwrap().is_empty());
        assert!(ssh.exec_calls.lock().unwrap().is_empty());
        let tmux_message = &tmux.notifications.lock().unwrap()[0].0;
        assert!(tmux_message.contains("viewers"));
        assert!(tmux_message.contains("palace-"));
    }

    #[tokio::test]
    async fn headless_context_returns_output_path() {
        let fixture = create_palace_fixture(false);
        let surface = harness(false, "darwin", Vec::new());

        let resolve = resolver(fixture.context.clone());
        run_palace_command(&resolve, &surface.context, None).await.unwrap();

        assert!(surface.exec_calls.lock().unwrap().is_empty());
        assert!(surface.notifications.lock().unwrap().is_empty());
        let outputs = surface.outputs.lock().unwrap();
        assert_eq!(outputs.len(), 1);
        assert!(std::path::Path::new(&outputs[0]).exists());
    }

    #[tokio::test]
    async fn unbound_identity_reports_without_generating() {
        let surface = harness(true, "darwin", Vec::new());
        let resolve: PalaceContextResolver = Arc::new(|_: &PalaceCommandContext| None);

        run_palace_command(&resolve, &surface.context, None).await.unwrap();

        assert!(surface.exec_calls.lock().unwrap().is_empty());
        let notifications = surface.notifications.lock().unwrap();
        assert!(notifications[0].0.contains("memory is not bound"));
        assert_eq!(notifications[0].1, PalaceNotificationLevel::Warning);
    }
}
