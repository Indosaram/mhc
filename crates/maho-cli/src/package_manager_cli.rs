#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PackageCommand { Install, Remove, Update, List }
pub struct PackageCommandOptions {
    pub command: PackageCommand, pub source: Option<String>, pub local: bool,
    pub project_trust_override: Option<bool>, pub help: bool,
    pub invalid_option: Option<String>, pub invalid_argument: Option<String>,
    pub missing_option_value: Option<String>, pub conflicting_options: Option<String>,
}
pub fn parse_package_command(args: &[String]) -> Option<PackageCommandOptions> {
    let command = match args.first()?.as_str() { "install" => PackageCommand::Install, "remove" | "uninstall" => PackageCommand::Remove, "update" => PackageCommand::Update, "list" => PackageCommand::List, _ => return None };
    let mut parsed = PackageCommandOptions { command, source: None, local: false, project_trust_override: None, help: false, invalid_option: None, invalid_argument: None, missing_option_value: None, conflicting_options: None };
    let mut index = 1;
    let mut extensions_flag = false;
    let mut extension_source = None;
    while index < args.len() {
        let argument = &args[index];
        match argument.as_str() {
            "--help" | "-h" => parsed.help = true,
            "--approve" | "-a" => parsed.project_trust_override = Some(true),
            "--no-approve" | "-na" => parsed.project_trust_override = Some(false),
            "--local" | "-l" if matches!(command, PackageCommand::Install | PackageCommand::Remove) => parsed.local = true,
            "--extensions" if command == PackageCommand::Update => extensions_flag = true,
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
    if extension_source.is_some() {
        if extensions_flag { parsed.conflicting_options.get_or_insert_with(|| "--extension cannot be combined with --self, --extensions, or --all".to_owned()); }
        if parsed.source.is_some() { parsed.conflicting_options.get_or_insert_with(|| "--extension cannot be combined with a positional source".to_owned()); }
        parsed.source = extension_source;
    } else if extensions_flag && parsed.source.is_some() {
        parsed.conflicting_options.get_or_insert_with(|| "positional update targets cannot be combined with --self, --extensions, or --all".to_owned());
    }
    Some(parsed)
}
