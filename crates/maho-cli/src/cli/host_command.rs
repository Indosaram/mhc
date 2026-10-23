#[derive(Debug, PartialEq, Eq)]
pub enum HostSubcommand {
    Ensure,
    Status,
    Stop,
    Handoff,
    ShardPath,
    Gc,
}
pub struct ParsedHostArgs {
    pub subcommand: HostSubcommand,
    pub spec_path: Option<String>,
    pub policy: String,
    pub socket: Option<String>,
    pub include_workers: bool,
    pub drain: bool,
    pub force: bool,
    pub expected_generation: Option<maho_rpc::host_runner::HostStopGeneration>,
    /// `stop --drain`: the generation to drain, by instance id.
    pub instance_id: Option<String>,
    /// `stop --drain`: how long to wait for the drained generation to exit.
    pub wait_ms: Option<u64>,
    /// `handoff --when idle ...`: the conditional handover's terms.
    pub handover: Option<super::host_handover_args::IdleHandoverTerms>,
    /// `status --all`: every endpoint under the agent dir.
    pub all: bool,
    /// `--json`: `shard-path` prints the whole JSON line instead of the bare socket path.
    pub json: bool,
    pub kind: Option<maho_rpc::host_endpoints::ShardKind>,
    pub owner: Option<String>,
    pub root: Option<String>,
    pub agent_dir: Option<String>,
}
/// senpi `parseHostStopGeneration` (maho-rpc owns the parser; host_runner carries its own mirror type).
fn parse_stop_generation(text: &str) -> Option<maho_rpc::host_runner::HostStopGeneration> {
    let parsed = maho_rpc::host_stop_generation::parse_host_stop_generation(text)?;
    Some(maho_rpc::host_runner::HostStopGeneration {
        instance_id: parsed.instance_id,
        generation: parsed.generation,
        pid: parsed.pid,
        process_start_time: parsed.process_start_time,
    })
}
const HANDOVER_FLAGS: [&str; 5] = [
    "--when",
    "--operation",
    "--if-instance",
    "--if-generation",
    "--target-build",
];
pub fn parse_host_args(args: &[String]) -> Result<ParsedHostArgs, String> {
    let app = maho_core::config::app_name();
    let name = args.first().map_or("", String::as_str);
    let subcommand = match name {
        "ensure" => HostSubcommand::Ensure,
        "status" => HostSubcommand::Status,
        "stop" => HostSubcommand::Stop,
        "handoff" => HostSubcommand::Handoff,
        "shard-path" => HostSubcommand::ShardPath,
        "gc" => HostSubcommand::Gc,
        _ => return Err(format!("Error: unknown host command \"{name}\".")),
    };
    let mut result = ParsedHostArgs {
        subcommand,
        spec_path: None,
        policy: "upgrade".to_owned(),
        socket: None,
        include_workers: false,
        drain: false,
        force: false,
        expected_generation: None,
        instance_id: None,
        wait_ms: None,
        handover: None,
        all: false,
        json: false,
        kind: None,
        owner: None,
        root: None,
        agent_dir: None,
    };
    let is_stop = result.subcommand == HostSubcommand::Stop;
    let is_handoff = result.subcommand == HostSubcommand::Handoff;
    let is_status = result.subcommand == HostSubcommand::Status;
    let is_shard = result.subcommand == HostSubcommand::ShardPath;
    let is_gc = result.subcommand == HostSubcommand::Gc;
    let mut wait_ms: Option<&String> = None;
    let mut handover = std::collections::BTreeMap::new();
    let mut index = 1;
    while index < args.len() {
        let flag = &args[index];
        let value = args.get(index + 1);
        match flag.as_str() {
            "--json" => result.json = true,
            "--all" if is_status => result.all = true,
            "--include-workers" => result.include_workers = true,
            "--drain" => result.drain = true,
            "--force" => result.force = true,
            "--kind"
                if is_shard && value.is_some_and(|value| matches!(value.as_str(), "p" | "i")) =>
            {
                result.kind =
                    value.and_then(|value| maho_rpc::host_endpoints::ShardKind::parse(value));
                index += 1;
            }
            "--owner" if is_shard && value.is_some() => {
                result.owner = value.cloned();
                index += 1;
            }
            "--root" if is_shard && value.is_some() => {
                result.root = value.cloned();
                index += 1;
            }
            "--agent-dir" if is_gc && value.is_some() => {
                result.agent_dir = value.cloned();
                index += 1;
            }
            "--launch-spec" if value.is_some() => {
                result.spec_path = value.cloned();
                index += 1;
            }
            "--socket" if value.is_some() => {
                result.socket = value.cloned();
                index += 1;
            }
            "--policy"
                if value.is_some_and(|value| {
                    matches!(value.as_str(), "upgrade" | "fallback" | "never")
                }) =>
            {
                result.policy = value.cloned().unwrap_or_default();
                index += 1;
            }
            _ if is_handoff && HANDOVER_FLAGS.contains(&flag.as_str()) && value.is_some() => {
                if let Some(value) = value {
                    handover.insert(flag.clone(), value.clone());
                }
                index += 1;
            }
            "--expected-generation" if is_stop && value.is_some() => {
                let parsed = value.and_then(|value| parse_stop_generation(value));
                if parsed.is_none() {
                    return Err("Error: --expected-generation needs instanceId, generation, pid and processStartTime (null where the platform records none).".to_owned());
                }
                result.expected_generation = parsed;
                index += 1;
            }
            "--instance" if is_stop && value.is_some() => {
                result.instance_id = value.cloned();
                index += 1;
            }
            "--wait-ms" if is_stop && value.is_some() => {
                wait_ms = value;
                index += 1;
            }
            _ => {
                return Err(format!(
                    "Error: unknown option \"{flag}\" for \"{app} host {name}\"."
                ));
            }
        }
        index += 1;
    }
    if is_shard && (result.kind.is_none() || result.owner.is_none()) {
        return Err(format!(
            "Error: \"{app} host shard-path\" needs --kind <p|i> and --owner <id>."
        ));
    }
    if (result.instance_id.is_some() || wait_ms.is_some()) && !result.drain {
        return Err(format!(
            "Error: \"{app} host stop\" takes --instance and --wait-ms only with --drain."
        ));
    }
    if result.expected_generation.is_some() && result.drain {
        return Err("Error: --expected-generation is for a hard stop, not --drain.".to_owned());
    }
    if let Some(text) = wait_ms {
        let parsed = (!text.is_empty() && text.chars().all(|c| c.is_ascii_digit()))
            .then(|| text.parse::<u64>().ok())
            .flatten()
            .filter(|value| (1..=9_007_199_254_740_991).contains(value));
        result.wait_ms = Some(parsed.ok_or_else(|| format!("Error: \"{app} host stop --wait-ms\" needs a positive whole number of milliseconds."))?);
    }
    if !handover.is_empty() {
        result.handover = Some(super::host_handover_args::handover_terms(&handover)?);
    }
    Ok(result)
}

const USAGE: &str = "usage: {app} host <ensure|status|stop|handoff|shard-path|gc> [options]

  ensure   [--launch-spec <file>] [--policy upgrade|fallback|never] [--socket <path>]
  status   [--include-workers] [--all] [--socket <path>]
  stop     [--drain [--instance <id>] [--wait-ms <n>]] [--force] [--expected-generation <json>] [--socket <path>]
  handoff  [--launch-spec <file>] [--socket <path>]
  shard-path  --kind <p|i> --owner <id> [--root <dir>] [--json]   (no host contact)
  gc       [--agent-dir <dir>]

  --json   accepted for symmetry; the answer is always one JSON line on stdout";

pub async fn run_host_command(args: &[String]) -> Result<i32, String> {
    use std::io::Write;
    let parsed = match parse_host_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            let usage = USAGE.replace("{app}", &maho_core::config::app_name());
            writeln!(std::io::stderr().lock(), "{message}\n{usage}")
                .map_err(|error| error.to_string())?;
            return Ok(maho_rpc::host_runner::HOST_EXIT_USAGE);
        }
    };
    let agent_dir = std::path::PathBuf::from(maho_core::config::get_agent_dir());
    let socket = parsed.socket.clone().unwrap_or_else(|| {
        maho_core::brand::env_value("RPC_SOCKET", &maho_core::config::current_env())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                agent_dir
                    .join("rpc/rpc.sock")
                    .to_string_lossy()
                    .into_owned()
            })
    });
    let outcome = match super::host_request::run(&parsed, &socket, &agent_dir).await {
        Ok(outcome) => outcome,
        Err(error) => {
            writeln!(std::io::stdout().lock(), "{}", error.payload())
                .map_err(|write| write.to_string())?;
            return Ok(error.exit_code());
        }
    };
    if parsed.subcommand == HostSubcommand::ShardPath && !parsed.json {
        let socket = outcome.payload["socket"].as_str().unwrap_or_default();
        writeln!(std::io::stdout().lock(), "{socket}").map_err(|error| error.to_string())?;
        return Ok(outcome.exit_code);
    }
    writeln!(std::io::stdout().lock(), "{}", outcome.payload).map_err(|error| error.to_string())?;
    Ok(outcome.exit_code)
}
