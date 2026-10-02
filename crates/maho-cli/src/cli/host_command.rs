#[derive(Debug, PartialEq, Eq)]
pub enum HostSubcommand { Ensure, Status, Stop, Handoff }
pub struct ParsedHostArgs { pub subcommand: HostSubcommand, pub spec_path: Option<String>, pub policy: String, pub socket: Option<String>, pub include_workers: bool, pub drain: bool, pub force: bool }
pub fn parse_host_args(args: &[String]) -> Result<ParsedHostArgs, String> {
    let name = args.first().map_or("", String::as_str);
    let subcommand = match name { "ensure" => HostSubcommand::Ensure, "status" => HostSubcommand::Status, "stop" => HostSubcommand::Stop, "handoff" => HostSubcommand::Handoff, _ => return Err(format!("Error: unknown host command \"{name}\".")) };
    let mut result = ParsedHostArgs { subcommand, spec_path: None, policy: "upgrade".to_owned(), socket: None, include_workers: false, drain: false, force: false };
    let mut index = 1;
    while index < args.len() {
        let flag = &args[index];
        let value = args.get(index + 1);
        match flag.as_str() {
            "--json" => {}, "--include-workers" => result.include_workers = true, "--drain" => result.drain = true, "--force" => result.force = true,
            "--launch-spec" if value.is_some() => { result.spec_path = value.cloned(); index += 1; },
            "--socket" if value.is_some() => { result.socket = value.cloned(); index += 1; },
            "--policy" if value.is_some_and(|value| matches!(value.as_str(), "upgrade" | "fallback" | "never")) => { result.policy = value.expect("policy present").clone(); index += 1; },
            _ => return Err(format!("Error: unknown option \"{flag}\" for \"mhc host {name}\".")),
        }
        index += 1;
    }
    Ok(result)
}
