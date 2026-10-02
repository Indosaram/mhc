//! Port of cli/args.ts. Unknown long flags remain available to native extensions.
use std::collections::BTreeMap;

#[derive(Clone, PartialEq, Eq)]
pub enum FlagValue { Boolean(bool), String(String) }
#[derive(Clone, PartialEq, Eq)]
pub enum Mode { Text, Json, Rpc }
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SessionRuntime { InProcess, Worker }
#[derive(Clone, PartialEq, Eq)]
pub struct Diagnostic { pub error: bool, pub message: String }
#[derive(Default, Clone)]
pub struct Args {
    pub provider: Option<String>, pub model: Option<String>, pub api_key: Option<String>,
    pub system_prompt: Option<String>, pub append_system_prompt: Vec<String>, pub thinking: Option<String>,
    pub continue_session: bool, pub resume: bool, pub help: bool, pub version: bool, pub mode: Option<Mode>,
    pub name: Option<String>, pub no_session: bool, pub session: Option<String>, pub session_id: Option<String>,
    pub fork: Option<String>, pub session_dir: Option<String>, pub models: Option<Vec<String>>,
    pub tools: Option<Vec<String>>, pub exclude_tools: Option<Vec<String>>, pub no_tools: bool,
    pub no_builtin_tools: bool, pub extensions: Vec<String>, pub no_extensions: bool, pub print: bool,
    pub export: Option<String>, pub no_skills: bool, pub skills: Vec<String>, pub prompt_templates: Vec<String>,
    pub no_prompt_templates: bool, pub themes: Vec<String>, pub use_theme: Option<String>, pub no_themes: bool,
    pub no_context_files: bool, pub list_models: Option<String>, pub list_tips: bool, pub offline: bool,
    pub tui_mode: Option<String>, pub verbose: bool, pub project_trust_override: Option<bool>, pub grok_neo: bool,
    pub multi_session: bool, pub auto_title_sessions: bool, pub listen: Option<String>,
    pub session_runtime: Option<SessionRuntime>, pub messages: Vec<String>, pub file_args: Vec<String>,
    pub unknown_flags: BTreeMap<String, FlagValue>, pub diagnostics: Vec<Diagnostic>,
}
pub fn normalize_session_name(value: &str) -> Option<&str> { let name = value.trim(); (!name.is_empty()).then_some(name) }
pub fn resolve_session_runtime(args: &Args) -> SessionRuntime {
    args.session_runtime.unwrap_or_else(|| if args.listen.as_deref().is_some_and(|s| s != "stdio://") { SessionRuntime::InProcess } else { SessionRuntime::Worker })
}
pub fn is_valid_thinking_level(value: &str) -> bool { matches!(value, "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max") }
impl Args { fn diagnostic(&mut self, error: bool, message: impl Into<String>) { self.diagnostics.push(Diagnostic { error, message: message.into() }); } }
fn positional(result: &mut Args, value: &str) {
    if let Some(file) = value.strip_prefix('@') { result.file_args.push(file.to_owned()); } else { result.messages.push(value.to_owned()); }
}
pub fn parse_args(args: &[String], grok_neo_enabled: bool) -> Result<Args, String> {
    let mut result = Args::default(); let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str(); let next = args.get(i + 1).map(String::as_str);
        match arg {
            "--" => { for value in &args[i + 1..] { positional(&mut result, value); } break; }
            "--help" | "-h" => result.help = true,
            "--version" | "-v" => result.version = true,
            "--continue" | "-c" => result.continue_session = true,
            "--resume" | "-r" => result.resume = true,
            "--no-session" => result.no_session = true,
            "--no-tools" | "-nt" => result.no_tools = true,
            "--no-builtin-tools" | "-nbt" => result.no_builtin_tools = true,
            "--no-extensions" | "-ne" => result.no_extensions = true,
            "--no-skills" | "-ns" => result.no_skills = true,
            "--no-prompt-templates" | "-np" => result.no_prompt_templates = true,
            "--no-themes" => result.no_themes = true,
            "--no-context-files" | "-nc" => result.no_context_files = true,
            "--verbose" => result.verbose = true,
            "--approve" | "-a" => result.project_trust_override = Some(true),
            "--no-approve" | "-na" => result.project_trust_override = Some(false),
            "--offline" => result.offline = true,
            "--grok-neo" if grok_neo_enabled => result.grok_neo = true,
            "--multi-session" => result.multi_session = true,
            "--auto-title-sessions" => result.auto_title_sessions = true,
            "--list-tips" => result.list_tips = true,
            "--print" | "-p" => {
                result.print = true;
                if let Some(value) = next.filter(|v| !v.starts_with('@') && (!v.starts_with('-') || v.starts_with("---"))) { result.messages.push(value.to_owned()); i += 1; }
            }
            "--name" | "-n" => if let Some(value) = next { result.name = Some(value.to_owned()); i += 1; } else { result.diagnostic(true, "--name requires a value"); },
            "--use-theme" => if let Some(value) = next.filter(|v| !v.starts_with('-')) { result.use_theme = Some(value.to_owned()); i += 1; } else { result.diagnostic(true, "--use-theme requires a theme name"); },
            "--tui-mode" => match next {
                Some("regular" | "fullscreen") => { result.tui_mode = next.map(str::to_owned); i += 1; }
                Some(value) if !value.starts_with('-') => { result.diagnostic(true, format!("Invalid TUI mode \"{value}\". Valid values: regular, fullscreen")); i += 1; }
                _ => result.diagnostic(true, "--tui-mode requires regular or fullscreen"),
            },
            "--session-runtime" => {
                if next.is_some_and(|v| !v.starts_with("--")) { i += 1; }
                match next { Some("in-process") => result.session_runtime = Some(SessionRuntime::InProcess), Some("worker") => result.session_runtime = Some(SessionRuntime::Worker), _ => result.diagnostic(true, "--session-runtime must be in-process or worker") }
            }
            "--listen" if result.mode == Some(Mode::Rpc) => if let Some(value) = next.filter(|v| !v.starts_with("--")) { result.listen = Some(value.to_owned()); result.multi_session = true; i += 1; } else { result.diagnostic(true, "--listen requires a value"); },
            "--list-models" => { result.list_models = Some(next.filter(|v| !v.starts_with('-') && !v.starts_with('@')).unwrap_or("").to_owned()); if result.list_models.as_deref() != Some("") { i += 1; } }
            "--provider" | "--model" | "--api-key" | "--system-prompt" | "--append-system-prompt" | "--mode" | "--session" | "--session-id" | "--fork" | "--session-dir" | "--models" | "--tools" | "-t" | "--exclude-tools" | "-xt" | "--thinking" | "--export" | "--extension" | "-e" | "--skill" | "--prompt-template" | "--theme" if next.is_some() => {
                if let Some(value) = next { i += 1; match arg {
                    "--provider" => { if let Some(error) = maho_ai::legacy_provider_ids::legacy_provider_id_rejection(value) { return Err(error); } result.provider = Some(value.to_owned()); }
                    "--model" => result.model = Some(value.to_owned()), "--api-key" => result.api_key = Some(value.to_owned()),
                    "--system-prompt" => result.system_prompt = Some(value.to_owned()), "--append-system-prompt" => result.append_system_prompt.push(value.to_owned()),
                    "--mode" => result.mode = match value { "text" => Some(Mode::Text), "json" => Some(Mode::Json), "rpc" => Some(Mode::Rpc), _ => None },
                    "--session" => result.session = Some(value.to_owned()), "--session-id" => result.session_id = Some(value.to_owned()), "--fork" => result.fork = Some(value.to_owned()), "--session-dir" => result.session_dir = Some(value.to_owned()),
                    "--models" => result.models = Some(value.split(',').map(|v| v.trim().to_owned()).collect()),
                    "--tools" | "-t" => result.tools = Some(value.split(',').map(str::trim).filter(|v| !v.is_empty()).map(str::to_owned).collect()),
                    "--exclude-tools" | "-xt" => result.exclude_tools = Some(value.split(',').map(str::trim).filter(|v| !v.is_empty()).map(str::to_owned).collect()),
                    "--thinking" => if is_valid_thinking_level(value) { result.thinking = Some(value.to_owned()); } else { result.diagnostic(false, format!("Invalid thinking level \"{value}\". Valid values: off, minimal, low, medium, high, xhigh, max")); },
                    "--export" => result.export = Some(value.to_owned()), "--extension" | "-e" => result.extensions.push(value.to_owned()),
                    "--skill" => result.skills.push(value.to_owned()), "--prompt-template" => result.prompt_templates.push(value.to_owned()), "--theme" => result.themes.push(value.to_owned()), _ => unreachable!(),
                } }
            }
            _ if arg.starts_with('@') => positional(&mut result, arg),
            _ if arg.starts_with("--") => {
                let name = &arg[2..];
                if let Some((name, value)) = name.split_once('=') { result.unknown_flags.insert(name.to_owned(), FlagValue::String(value.to_owned())); }
                else if let Some(value) = next.filter(|v| !v.starts_with('-') && !v.starts_with('@')) { result.unknown_flags.insert(name.to_owned(), FlagValue::String(value.to_owned())); i += 1; }
                else { result.unknown_flags.insert(name.to_owned(), FlagValue::Boolean(true)); }
            }
            _ if arg.starts_with('-') => result.diagnostic(true, format!("Unknown option: {arg}")),
            _ => positional(&mut result, arg),
        }
        i += 1;
    }
    Ok(result)
}
pub fn help_text(grok_neo_enabled: bool, extension_flags: &str) -> String {
    include_str!("help.txt").replace("{grok_neo}", if grok_neo_enabled { "  --grok-neo                     Launch the experimental grok interactive chrome\n" } else { "" }).replace("{extension_flags}", extension_flags)
}
