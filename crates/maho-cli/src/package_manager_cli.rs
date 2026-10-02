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
