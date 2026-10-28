//! MIT profile (`--no-default-features`): first-run setup from `maho-onboarding`
//! (docs/cleanroom/specs/onboarding.md) — `mhc setup` and the gate that runs the setup prompts
//! before the first interactive session when no model is usable yet. The provider/model catalog
//! and credentials come from the senpi model runtime (`models.json` + `auth.json`, offline); the
//! key goes to `auth.json`, the default model to `settings.json` and, through the `maho-config`
//! writer, to the `setup` model profile in `~/.omo/omo.jsonc`. The `omo` build does not use this.

use maho_onboarding::{ProviderChoice, SetupHost};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;

/// The catalog seen by setup, read once.
pub struct CliSetupHost {
    catalog: BTreeMap<String, Vec<String>>,
    credentialed: BTreeSet<String>,
    agent_dir: PathBuf,
    home: PathBuf,
    cwd: PathBuf,
}

impl CliSetupHost {
    /// Reads the catalog and the providers with usable credentials (offline).
    pub fn load() -> Result<Self, String> {
        let agent_dir = PathBuf::from(maho_core::config::get_agent_dir());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        let (all, available) = runtime.block_on(async {
            let models = maho_core::model_runtime::ModelRuntime::create(
                maho_core::model_runtime::CreateModelRuntimeOptions {
                    models_path: Some(agent_dir.join("models.json")),
                    auth_path: Some(agent_dir.join("auth.json")),
                    agent_dir: Some(agent_dir.clone()),
                    allow_model_network: Some(false),
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| error.to_string())?;
            let all = models.get_models(None);
            let available = models.get_available(None).await.unwrap_or_default();
            Ok::<_, String>((all, available))
        })?;
        let mut catalog: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for model in all {
            let ids = catalog.entry(model.provider.clone()).or_default();
            if !ids.contains(&model.id) {
                ids.push(model.id);
            }
        }
        let credentialed = available.into_iter().map(|m| m.provider).collect();
        Ok(Self {
            catalog,
            credentialed,
            agent_dir,
            home: PathBuf::from(maho_core::config::home_dir()),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        })
    }

    /// Whether any model can be used now.
    pub fn has_usable_model(&self) -> bool {
        !self.credentialed.is_empty()
    }

    fn agent_home(&self) -> maho_agent_home::AgentHome {
        maho_agent_home::AgentHome::new(&self.home, &self.agent_dir)
    }
}

impl SetupHost for CliSetupHost {
    fn providers(&self) -> Vec<ProviderChoice> {
        let mut list: Vec<ProviderChoice> = self
            .catalog
            .keys()
            .map(|id| ProviderChoice {
                id: id.clone(),
                name: id.clone(),
                has_credential: self.credentialed.contains(id),
            })
            .collect();
        // Providers that already work come first; the rest stay alphabetical.
        list.sort_by_key(|p| !p.has_credential);
        list
    }

    fn models(&self, provider: &str) -> Vec<String> {
        self.catalog.get(provider).cloned().unwrap_or_default()
    }

    fn store_key(&mut self, provider: &str, key: &str) -> Result<(), String> {
        std::fs::create_dir_all(&self.agent_dir).map_err(|error| error.to_string())?;
        let auth = self.agent_dir.join("auth.json");
        maho_core::auth_storage::AuthStorage::create(&auth.to_string_lossy()).set(
            provider,
            Some(serde_json::json!({"type": "api_key", "key": key})),
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&auth, std::fs::Permissions::from_mode(0o600));
        }
        self.credentialed.insert(provider.to_owned());
        Ok(())
    }

    fn set_default_model(&mut self, provider: &str, model: &str) -> Result<(), String> {
        let mut settings = maho_core::settings_manager::SettingsManager::create(
            &self.cwd.to_string_lossy(),
            &self.agent_dir.to_string_lossy(),
            &self.home.to_string_lossy(),
            false,
        );
        let mut values = serde_json::Map::new();
        values.insert("defaultProvider".into(), provider.into());
        values.insert("defaultModel".into(), model.into());
        settings.set(
            maho_core::settings_manager::SettingsScope::Global,
            &values,
        )?;
        maho_onboarding::apply::write_model_profile(&maho_config::process_env(), provider, model)
            .map(|_| ())
    }

    fn mark_complete(&mut self, version: u32, provider: &str, model: &str) -> Result<(), String> {
        let home = self.agent_home();
        home.bootstrap().map_err(|error| error.to_string())?;
        home.mark_setup_complete(version, Some(provider), Some(&format!("{provider}/{model}")))
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

/// Prompts on stderr, answers from stdin; keys are read without echo.
pub struct TerminalPrompter;

impl maho_onboarding::Prompter for TerminalPrompter {
    fn say(&mut self, text: &str) {
        eprintln!("{text}");
    }
    fn ask(&mut self, prompt: &str) -> Option<String> {
        eprint!("{prompt}");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line.trim_end_matches(['\r', '\n']).to_owned()),
        }
    }
    fn ask_secret(&mut self, prompt: &str) -> Option<String> {
        if std::io::stdin().is_terminal() {
            rpassword::prompt_password(prompt).ok()
        } else {
            self.ask(prompt)
        }
    }
}

fn print(output: &maho_onboarding::cli::Output) {
    if !output.stdout.is_empty() {
        maho_core::output_guard::maho_write_stdout(&format!("{}\n", output.stdout.trim_end()));
    }
    if !output.stderr.is_empty() {
        maho_core::output_guard::maho_write_stderr(&format!("{}\n", output.stderr.trim_end()));
    }
}

/// `mhc setup ...`; returns the exit code.
pub fn run_setup_command(args: &[String]) -> Result<i32, String> {
    let parsed = maho_onboarding::cli::parse(args);
    if matches!(&parsed, Ok(a) if a.help || a.check) || parsed.is_err() {
        // Help, --check and usage errors need no catalog.
        let mut host = EmptyHost;
        return Ok(run_with(args, &mut host));
    }
    let mut host = CliSetupHost::load()?;
    Ok(run_with(args, &mut host))
}

fn run_with(args: &[String], host: &mut dyn SetupHost) -> i32 {
    let env = |key: &str| std::env::var(key).ok();
    let mut read_stdin = || {
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
            .map(|_| text)
            .map_err(|error| error.to_string())
    };
    let stored_version = maho_agent_home::AgentHome::new(
        &PathBuf::from(maho_core::config::home_dir()),
        &PathBuf::from(maho_core::config::get_agent_dir()),
    )
    .setup_version();
    let output = maho_onboarding::cli::run(
        args,
        host,
        &mut TerminalPrompter,
        maho_onboarding::cli::Io {
            stdin_tty: std::io::stdin().is_terminal(),
            stdout_tty: std::io::stdout().is_terminal(),
            stored_version,
            env: &env,
            read_stdin: &mut read_stdin,
        },
    );
    print(&output);
    output.code
}

struct EmptyHost;

impl SetupHost for EmptyHost {
    fn providers(&self) -> Vec<ProviderChoice> {
        Vec::new()
    }
    fn models(&self, _provider: &str) -> Vec<String> {
        Vec::new()
    }
    fn store_key(&mut self, _: &str, _: &str) -> Result<(), String> {
        Err("no catalog".into())
    }
    fn set_default_model(&mut self, _: &str, _: &str) -> Result<(), String> {
        Err("no catalog".into())
    }
    fn mark_complete(&mut self, _: u32, _: &str, _: &str) -> Result<(), String> {
        Err("no catalog".into())
    }
}

/// The first-run gate (spec §2): before an interactive session on a terminal, run the setup
/// prompts when the stored setup version is stale and no model is usable; when a model is already
/// usable the stored version is only advanced. Never fails the session: problems are reported and
/// the session starts anyway.
pub fn first_run_gate(parsed: &super::args::Args) {
    let interactive = parsed.mode.is_none()
        && !parsed.print
        && parsed.messages.is_empty()
        && parsed.list_models.is_none()
        && parsed.export.is_none();
    if !interactive {
        return;
    }
    let home = maho_agent_home::AgentHome::new(
        &PathBuf::from(maho_core::config::home_dir()),
        &PathBuf::from(maho_core::config::get_agent_dir()),
    );
    let stored = home.setup_version();
    let enabled = {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let loaded = maho_config::load_native(&cwd, &maho_config::process_env());
        maho_onboarding::steps::enabled_in(&loaded.view)
    };
    let steps = maho_onboarding::select(
        stored,
        &maho_onboarding::SelectOptions {
            is_tty: std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
            resuming: parsed.continue_session
                || parsed.resume
                || parsed.session.is_some()
                || parsed.fork.is_some(),
            skip_env: std::env::var(maho_onboarding::SKIP_ENV).ok(),
            enabled,
            force: false,
        },
    );
    if steps.is_empty() {
        return;
    }
    let mut host = match CliSetupHost::load() {
        Ok(host) => host,
        Err(error) => {
            eprintln!("Warning: first-run setup skipped: {error}");
            return;
        }
    };
    if host.has_usable_model() || parsed.model.is_some() || parsed.provider.is_some() {
        if let Err(error) =
            home.mark_setup_complete(maho_onboarding::CURRENT_SETUP_VERSION, None, None)
        {
            eprintln!("Warning: cannot record setup state: {error}");
        }
        return;
    }
    let output = maho_onboarding::cli::interactive(&mut host, &mut TerminalPrompter, &steps, None);
    print(&output);
}
