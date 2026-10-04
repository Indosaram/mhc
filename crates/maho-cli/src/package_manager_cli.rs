#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PackageCommand { Install, Remove, Update, List }
#[derive(Debug, PartialEq, Eq)]
pub enum UpdateTarget { SelfOnly, Extensions { source: Option<String> }, Models, All }
pub fn package_command_usage(command: PackageCommand) -> &'static str {
    match command {
        PackageCommand::Install => "mhc install <source> [-l] [--approve|--no-approve]",
        PackageCommand::Remove => "mhc remove <source> [-l] [--approve|--no-approve]",
        PackageCommand::Update => "mhc update [source|self|mhc] [--self|--extensions|--models|--all] [--extension <source>] [--approve|--no-approve] [--force]",
        PackageCommand::List => "mhc list [--approve|--no-approve]",
    }
}
pub fn package_command_help(command: PackageCommand) -> String {
    let body = match command {
        PackageCommand::Install => "Install a package and add it to settings.\n\nOptions:\n  -l, --local       Install project-locally (.maho/settings.json)\n  -a, --approve     Trust project-local files for this command\n  -na, --no-approve Ignore project-local files for this command\n\nExamples:\n  mhc install npm:@foo/bar\n  mhc install git:github.com/user/repo\n  mhc install git:git@github.com:user/repo\n  mhc install https://github.com/user/repo\n  mhc install ssh://git@github.com/user/repo\n  mhc install ./local/path\n",
        PackageCommand::Remove => "Remove a package and its source from settings.\nAlias: mhc uninstall <source> [-l]\n\nOptions:\n  -l, --local       Remove from project settings (.maho/settings.json)\n  -a, --approve     Trust project-local files for this command\n  -na, --no-approve Ignore project-local files for this command\n\nExamples:\n  mhc remove npm:@foo/bar\n  mhc uninstall npm:@foo/bar\n",
        PackageCommand::Update => "Update resource packages or model catalogs.\n\nOptions:\n  --extensions            Update installed packages only\n  --models                Refresh model catalogs only\n  --extension <source>    Update one package only\n  -a, --approve           Trust project-local files for this command\n  -na, --no-approve       Ignore project-local files for this command\n\nNative self-update is replaced by mhc import-omo.\n",
        PackageCommand::List => "List installed packages from user and project settings.\n\nOptions:\n  -a, --approve      Trust project-local files for this command\n  -na, --no-approve  Ignore project-local files for this command\n",
    };
    format!("Usage:\n  {}\n\n{body}\n", package_command_usage(command))
}
pub fn config_command_help() -> &'static str {
    "Usage:\n  mhc config [-l] [--approve|--no-approve]\n\nOpen the resource configuration TUI to enable or disable package resources.\nWithout -l, starts in global settings (~/.maho/agent/settings.json).\nPress Tab in the TUI to switch between global and project-local modes.\n\nOptions:\n  -l, --local       Edit project overrides (.maho/settings.json)\n  -a, --approve     Trust project-local files for this command with -l\n  -na, --no-approve Ignore project-local files for this command with -l\n\n"
}
pub struct ConfigCommandOptions { pub local: bool, pub project_trust_override: Option<bool>, pub help: bool }
pub fn parse_config_command(args: &[String]) -> Result<Option<ConfigCommandOptions>, String> {
    if args.first().map(String::as_str) != Some("config") { return Ok(None); }
    let mut options = ConfigCommandOptions { local: false, project_trust_override: None, help: args[1..].iter().any(|argument| matches!(argument.as_str(), "-h" | "--help")) };
    if options.help { return Ok(Some(options)); }
    for argument in &args[1..] {
        match argument.as_str() {
            "-l" | "--local" => options.local = true,
            "-a" | "--approve" => options.project_trust_override = Some(true),
            "-na" | "--no-approve" => options.project_trust_override = Some(false),
            _ if argument.starts_with('-') => return Err(format!("Unknown option {argument} for \"config\".")),
            _ => return Err(format!("Unexpected argument {argument}.")),
        }
    }
    Ok(Some(options))
}
pub struct PackageCommandOptions {
    pub command: PackageCommand, pub source: Option<String>, pub local: bool,
    pub update_target: Option<UpdateTarget>, pub show_extensions_skipped_note: bool,
    pub force: bool, pub omo_local_update_worker: bool,
    pub project_trust_override: Option<bool>, pub help: bool,
    pub invalid_option: Option<String>, pub invalid_argument: Option<String>,
    pub missing_option_value: Option<String>, pub conflicting_options: Option<String>,
}
pub fn parse_package_command(args: &[String]) -> Option<PackageCommandOptions> {
    let command = match args.first()?.as_str() { "install" => PackageCommand::Install, "remove" | "uninstall" => PackageCommand::Remove, "update" => PackageCommand::Update, "list" => PackageCommand::List, _ => return None };
    let mut parsed = PackageCommandOptions { command, source: None, local: false, update_target: None, show_extensions_skipped_note: false, force: false, omo_local_update_worker: false, project_trust_override: None, help: false, invalid_option: None, invalid_argument: None, missing_option_value: None, conflicting_options: None };
    let mut index = 1;
    let mut extensions_flag = false;
    let mut self_flag = false;
    let mut models_flag = false;
    let mut all_flag = false;
    let mut extension_source = None;
    while index < args.len() {
        let argument = &args[index];
        match argument.as_str() {
            "--help" | "-h" => parsed.help = true,
            "--approve" | "-a" => parsed.project_trust_override = Some(true),
            "--no-approve" | "-na" => parsed.project_trust_override = Some(false),
            "--local" | "-l" if matches!(command, PackageCommand::Install | PackageCommand::Remove) => parsed.local = true,
            "--extensions" if command == PackageCommand::Update => extensions_flag = true,
            "--self" if command == PackageCommand::Update => self_flag = true,
            "--models" if command == PackageCommand::Update => models_flag = true,
            "--all" if command == PackageCommand::Update => all_flag = true,
            "--force" if command == PackageCommand::Update => parsed.force = true,
            "--omo-local-update-worker" if command == PackageCommand::Update => parsed.omo_local_update_worker = true,
            "--extension" if command == PackageCommand::Update => {
                if let Some(value) = args.get(index + 1).filter(|value| !value.is_empty() && !value.starts_with('-')) {
                    if extension_source.is_some() { parsed.conflicting_options.get_or_insert_with(|| "--extension can only be provided once".to_owned()); }
                    else { extension_source = Some(value.clone()); }
                    index += 1;
                } else { parsed.missing_option_value.get_or_insert_with(|| argument.clone()); }
            }
            _ if argument.starts_with('-') => { parsed.invalid_option.get_or_insert_with(|| argument.clone()); }
            _ => {
                if parsed.source.as_deref().is_none_or(str::is_empty) { parsed.source = Some(argument.clone()); }
                else { parsed.invalid_argument.get_or_insert_with(|| argument.clone()); }
            }
        }
        index += 1;
    }
    if command == PackageCommand::Update {
        let source_present = parsed.source.as_ref().is_some_and(|source| !source.is_empty());
        if all_flag && (self_flag || extensions_flag || models_flag || extension_source.is_some()) {
            parsed.conflicting_options.get_or_insert_with(|| "--all cannot be combined with --self, --extensions, --models, or --extension".to_owned());
        }
        if all_flag && source_present { parsed.conflicting_options.get_or_insert_with(|| "--all cannot be combined with a positional source".to_owned()); }
        parsed.update_target = Some(if models_flag {
            if self_flag || extensions_flag || all_flag || extension_source.is_some() { parsed.conflicting_options.get_or_insert_with(|| "--models cannot be combined with --self, --extensions, --all, or --extension".to_owned()); }
            if source_present { parsed.conflicting_options.get_or_insert_with(|| "--models cannot be combined with a positional source".to_owned()); }
            UpdateTarget::Models
        } else if let Some(source) = extension_source {
            if self_flag || extensions_flag || all_flag { parsed.conflicting_options.get_or_insert_with(|| "--extension cannot be combined with --self, --extensions, or --all".to_owned()); }
            if source_present { parsed.conflicting_options.get_or_insert_with(|| "--extension cannot be combined with a positional source".to_owned()); }
            UpdateTarget::Extensions { source: Some(source) }
        } else if source_present {
            let source = parsed.source.clone().expect("source present");
            if matches!(source.as_str(), "self" | "pi" | "mhc") {
                if extensions_flag { UpdateTarget::All } else { UpdateTarget::SelfOnly }
            } else {
                if extensions_flag || self_flag || all_flag { parsed.conflicting_options.get_or_insert_with(|| "positional update targets cannot be combined with --self, --extensions, or --all".to_owned()); }
                UpdateTarget::Extensions { source: Some(source) }
            }
        } else if all_flag || (self_flag && extensions_flag) { UpdateTarget::All }
        else if self_flag { UpdateTarget::SelfOnly }
        else if extensions_flag { UpdateTarget::Extensions { source: None } }
        else { parsed.show_extensions_skipped_note = true; UpdateTarget::SelfOnly });
    }
    Some(parsed)
}

async fn command_settings(trust_override: Option<bool>, saved_only: bool) -> Result<(String, String, maho_core::settings_manager::SettingsManager), String> {
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?.to_string_lossy().into_owned();
    let agent_dir = maho_core::config::get_agent_dir();
    let home = maho_core::config::home_dir();
    let global = maho_core::settings_manager::SettingsManager::create(&cwd, &agent_dir, &home, false);
    let store = maho_core::trust_manager::ProjectTrustStore::new(&agent_dir);
    let trusted = if saved_only {
        trust_override.unwrap_or(store.get(&cwd)? == Some(true))
    } else {
        let context = crate::cli::project_trust::CliProjectTrustContext {
            cwd: cwd.clone(), mode: maho_core::project_trust::AppMode::Print,
            ui_available: false, selector: Box::new(|_, _| None),
        };
        maho_core::project_trust::resolve_project_trusted(maho_core::project_trust::ResolveProjectTrustedOptions {
            cwd: &cwd, trust_store: &store, trust_override,
            default_project_trust: global.get_string("defaultProjectTrust").as_deref().and_then(maho_core::project_trust::DefaultProjectTrust::parse),
            emit_project_trust: None, project_trust_context: &context, on_extension_error: None,
        }).await
    };
    let settings = maho_core::settings_manager::SettingsManager::create(&cwd, &agent_dir, &home, trusted);
    for error in settings.errors() { eprintln!("Resource command settings error: {error:?}"); }
    Ok((cwd, agent_dir, settings))
}

pub async fn run_package_command(options: PackageCommandOptions) -> Result<(), String> {
    if options.command == PackageCommand::Update && options.update_target == Some(UpdateTarget::Models) {
        let agent_dir = std::path::PathBuf::from(maho_core::config::get_agent_dir());
        let mut runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
            auth_path: Some(agent_dir.join("auth.json")), models_path: Some(agent_dir.join("models.json")), ..Default::default()
        });
        let result = tokio::time::timeout(std::time::Duration::from_secs(15), runtime.refresh(maho_ai::models::ModelsRefreshOptions {
            allow_network: Some(true), force: Some(true), ..Default::default()
        })).await.map_err(|_| "Model catalog refresh timed out.".to_owned())?;
        if result.aborted { return Err("Model catalog refresh timed out.".into()); }
        if !result.errors.is_empty() {
            return Err(format!("Could not refresh model catalogs: {}", result.errors.iter().map(|(provider, error)| format!("{provider}: {error}")).collect::<Vec<_>>().join("; ")));
        }
        maho_core::output_guard::maho_write_stdout("Model catalogs refreshed\n");
        return Ok(());
    }
    let (cwd, agent_dir, mut settings) = command_settings(options.project_trust_override, options.command == PackageCommand::Update).await?;
    let mut manager = maho_core::package_manager::DefaultPackageManager::new(maho_core::package_manager::PackageManagerOptions {
        cwd: &cwd, agent_dir: &agent_dir, settings_manager: &mut settings,
    });
    manager.set_progress_callback(Some(std::sync::Arc::new(|event| {
        if event.event_type == maho_core::package_manager::ProgressEventType::Start {
            if let Some(message) = event.message { maho_core::output_guard::maho_write_stdout(&format!("{message}\n")); }
        }
    })));
    execute_package_command(&options, &mut manager).await
}

/// The caller's settings manager is borrowed by the real resource manager throughout execution.
pub async fn execute_package_command(options: &PackageCommandOptions, manager: &mut maho_core::package_manager::DefaultPackageManager<'_>) -> Result<(), String> {
    use maho_core::package_manager::{InstalledSourceScope, PackageOptions};
    let text = match options.command {
        PackageCommand::Install => {
            let source = options.source.as_deref().ok_or("Missing install source")?;
            manager.install_and_persist(source, Some(PackageOptions { local: options.local })).await.map_err(|error| error.to_string())?;
            format!("Installed {source}\n")
        }
        PackageCommand::Remove => {
            let source = options.source.as_deref().ok_or("Missing remove source")?;
            if !manager.remove_and_persist(source, Some(PackageOptions { local: options.local })).await.map_err(|error| error.to_string())? {
                return Err(format!("No matching package found for {source}"));
            }
            format!("Removed {source}\n")
        }
        PackageCommand::List => {
            let packages = manager.list_configured_packages().map_err(|error| error.to_string())?;
            let mut text = String::new();
            for (scope, title) in [(InstalledSourceScope::User, "User packages:"), (InstalledSourceScope::Project, "Project packages:")] {
                let entries = packages.iter().filter(|package| package.scope == scope).collect::<Vec<_>>();
                if entries.is_empty() { continue; }
                if !text.is_empty() { text.push('\n'); }
                text.push_str(&format!("{title}\n"));
                for package in entries {
                    text.push_str(&format!("  {}{}\n", package.source, if package.filtered { " (filtered)" } else { "" }));
                    if let Some(path) = &package.installed_path { text.push_str(&format!("    {path}\n")); }
                }
            }
            if text.is_empty() { text.push_str("No packages installed.\n"); }
            text
        }
        PackageCommand::Update => {
            if options.omo_local_update_worker { return Err("Native self-update is replaced by mhc import-omo".into()); }
            match options.update_target.as_ref().unwrap_or(&UpdateTarget::SelfOnly) {
                UpdateTarget::Extensions { source } => {
                    manager.update(source.as_deref()).await.map_err(|error| error.to_string())?;
                    source.as_ref().map_or_else(|| "Updated packages\n".into(), |source| format!("Updated {source}\n"))
                }
                UpdateTarget::All => {
                    manager.update(None).await.map_err(|error| error.to_string())?;
                    return Err("Updated resource packages; native self-update is replaced by mhc import-omo".into());
                }
                UpdateTarget::SelfOnly => return Err("Native self-update is replaced by mhc import-omo".into()),
                UpdateTarget::Models => return Err("Model catalog refresh requires the model-runtime owner's refresh API".into()),
            }
        }
    };
    maho_core::output_guard::maho_write_stdout(&text);
    Ok(())
}

pub async fn run_config_command(options: ConfigCommandOptions) -> Result<(), String> {
    use maho_core::package_manager::{DefaultPackageManager, PackageManagerOptions};
    let (cwd, agent_dir, mut settings) = command_settings(options.project_trust_override, false).await?;
    if options.local && !settings.is_project_trusted() { return Err("Project is not trusted. Use --approve to modify local resource config.".into()); }
    let mut global_settings = maho_core::settings_manager::SettingsManager::create(&cwd, &agent_dir, &maho_core::config::home_dir(), false);
    let global = DefaultPackageManager::new(PackageManagerOptions { cwd: &cwd, agent_dir: &agent_dir, settings_manager: &mut global_settings }).resolve(None).await.map_err(|error| error.to_string())?;
    let project = if settings.is_project_trusted() {
        DefaultPackageManager::new(PackageManagerOptions { cwd: &cwd, agent_dir: &agent_dir, settings_manager: &mut settings }).resolve(None).await.map_err(|error| error.to_string())?
    } else { global.clone() };
    select_config(settings, cwd, agent_dir, options.local, global, project)
}

fn select_config(settings: maho_core::settings_manager::SettingsManager, cwd: String, agent_dir: String, local: bool, global: maho_core::package_manager::ResolvedPaths, project: maho_core::package_manager::ResolvedPaths) -> Result<(), String> {
    use std::{cell::{Cell, RefCell}, rc::Rc};
    use maho_interactive::{components::config_selector::{ConfigSelectorComponent, ConfigWriteScope}, tui_renderer::{create_interactive_tui, InteractiveTuiOptions}};
    use maho_tui::{terminal::{ProcessTerminal, Terminal}, tui::Component};
    let theme = crate::cli::startup_ui::resolve_startup_theme(settings.get_string("theme").as_deref(), std::env::var("COLORFGBG").ok().as_deref())?;
    let show_hardware_cursor = settings.get_bool("showHardwareCursor").unwrap_or(false);
    let clear_on_shrink = settings.get_bool("clearOnShrink").unwrap_or(false);
    let available = settings.is_project_trusted();
    let closed = Rc::new(Cell::new(false)); let close = closed.clone(); let exit = closed.clone();
    let switch = Rc::new(Cell::new(false)); let toggle = switch.clone();
    let host = ConfigSettings { settings, error: Rc::new(RefCell::new(None)) }; let write_error = host.error.clone();
    let mut terminal = ProcessTerminal::default();
    let selector = Rc::new(RefCell::new(ConfigSelectorComponent::new(&theme,
        std::collections::BTreeMap::from([(ConfigWriteScope::Global, global), (ConfigWriteScope::Project, project)]),
        Box::new(host), &cwd, &agent_dir, &maho_core::config::config_dir_name(),
        Box::new(move || close.set(true)), Box::new(move || exit.set(true)), Box::new(|| {}),
        Some(usize::from(terminal.rows())), if local { ConfigWriteScope::Project } else { ConfigWriteScope::Global }, available)));
    selector.borrow_mut().set_switch_mode(Box::new(move || toggle.set(true)));
    let component: Rc<RefCell<dyn Component>> = selector.clone();
    let mut screen = create_interactive_tui(InteractiveTuiOptions {
        tui_mode: maho_interactive::tui_renderer::TuiMode::Regular,
        show_hardware_cursor, bottom_shortcut: String::new(),
    }, theme);
    screen.base_mut().set_clear_on_shrink(clear_on_shrink);
    screen.base_mut().add_child(component.clone()); screen.base_mut().set_focus(Some(component));
    let input = Rc::new(RefCell::new(Vec::<String>::new())); let captured = input.clone();
    screen.before_terminal_start(&mut terminal, false, false);
    terminal.start(Box::new(move |chunk| captured.borrow_mut().push(chunk.into())), Box::new(|| {}));
    let clock = std::time::Instant::now();
    let result = (|| {
        while !closed.get() {
            for chunk in std::mem::take(&mut *input.borrow_mut()) { selector.borrow_mut().handle_input(&chunk); }
            if switch.replace(false) && available { selector.borrow_mut().switch_write_scope(); }
            if let Some(error) = write_error.borrow_mut().take() { return Err(error); }
            let now = u64::try_from(clock.elapsed().as_millis()).map_err(|error| error.to_string())?;
            screen.base_mut().request_render(false, now); screen.do_render(&mut terminal);
            terminal.pump(16).map_err(|error| error.to_string())?;
        }
        Ok(())
    })();
    screen.before_terminal_stop(&mut terminal, false);
    let stopped = terminal.stop().map_err(|error| error.to_string());
    screen.after_terminal_stop(&mut terminal, false);
    result.and(stopped)
}

struct ConfigSettings {
    settings: maho_core::settings_manager::SettingsManager,
    error: std::rc::Rc<std::cell::RefCell<Option<String>>>,
}
impl ConfigSettings {
    fn write(&mut self, scope: maho_core::settings_manager::SettingsScope, key: &str, value: serde_json::Value) {
        if let Err(error) = self.settings.set(scope, &maho_core::settings_manager::Settings::from(serde_json::Map::from_iter([(key.into(), value)]))) {
            *self.error.borrow_mut() = Some(error);
        }
    }
}
impl maho_interactive::components::config_selector::ConfigSettingsHost for ConfigSettings {
    fn get_global_settings(&self) -> serde_json::Value { serde_json::json!(self.settings.get_global()) }
    fn get_project_settings(&self) -> serde_json::Value { serde_json::json!(self.settings.get_project()) }
    fn set_packages(&mut self, values: &[serde_json::Value]) { self.write(maho_core::settings_manager::SettingsScope::Global, "packages", serde_json::json!(values)); }
    fn set_project_packages(&mut self, values: &[serde_json::Value]) { self.write(maho_core::settings_manager::SettingsScope::Project, "packages", serde_json::json!(values)); }
    fn set_extension_paths(&mut self, values: &[String]) { self.write(maho_core::settings_manager::SettingsScope::Global, "extensions", serde_json::json!(values)); }
    fn set_project_extension_paths(&mut self, values: &[String]) { self.write(maho_core::settings_manager::SettingsScope::Project, "extensions", serde_json::json!(values)); }
    fn set_skill_paths(&mut self, values: &[String]) { self.write(maho_core::settings_manager::SettingsScope::Global, "skills", serde_json::json!(values)); }
    fn set_project_skill_paths(&mut self, values: &[String]) { self.write(maho_core::settings_manager::SettingsScope::Project, "skills", serde_json::json!(values)); }
    fn set_prompt_template_paths(&mut self, values: &[String]) { self.write(maho_core::settings_manager::SettingsScope::Global, "prompts", serde_json::json!(values)); }
    fn set_project_prompt_template_paths(&mut self, values: &[String]) { self.write(maho_core::settings_manager::SettingsScope::Project, "prompts", serde_json::json!(values)); }
    fn set_theme_paths(&mut self, values: &[String]) { self.write(maho_core::settings_manager::SettingsScope::Global, "themes", serde_json::json!(values)); }
    fn set_project_theme_paths(&mut self, values: &[String]) { self.write(maho_core::settings_manager::SettingsScope::Project, "themes", serde_json::json!(values)); }
}
