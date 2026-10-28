//! `mhc gateway list [--json]` and `mhc gateway send <target> <message…> [--steer] [--from <user>]`
//! (spec §7).

use crate::store::{Delivery, Mode, Sender, Store};

pub const USAGE: &str = "Usage:\n  mhc gateway list [--json]\n  mhc gateway send <session> <message...> [--steer] [--from <user>]\n\n<session> is a live session id, its name, or a unique id prefix (4+ characters).";

/// What a CLI call prints and its exit code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

fn ok(stdout: String) -> CliOutput {
    CliOutput {
        code: 0,
        stdout,
        stderr: String::new(),
    }
}

fn fail(code: i32, stderr: String) -> CliOutput {
    CliOutput {
        code,
        stdout: String::new(),
        stderr,
    }
}

/// Runs `args` (after `gateway`) against `store`; `env` supplies `USER`/`USERNAME`.
pub fn run(args: &[String], store: &Store, env: &dyn Fn(&str) -> Option<String>) -> CliOutput {
    match args.first().map(String::as_str) {
        Some("list") => list(&args[1..], store),
        Some("send") => send(&args[1..], store, env),
        Some("help" | "--help" | "-h") => ok(USAGE.to_owned()),
        Some(other) => fail(2, format!("unknown gateway command \"{other}\"\n{USAGE}")),
        None => fail(2, USAGE.to_owned()),
    }
}

fn list(args: &[String], store: &Store) -> CliOutput {
    let json = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => return fail(2, USAGE.to_owned()),
    };
    let live = store.live();
    if json {
        return ok(serde_json::to_string_pretty(&live).unwrap_or_else(|_| "[]".into()));
    }
    if live.is_empty() {
        return ok("No live sessions.".into());
    }
    let lines: Vec<String> = live
        .iter()
        .map(|endpoint| {
            format!(
                "{}\t{}\t{}\tpid {}",
                endpoint.session_id,
                endpoint.name.as_deref().unwrap_or("-"),
                endpoint.cwd,
                endpoint.pid
            )
        })
        .collect();
    ok(lines.join("\n"))
}

fn send(args: &[String], store: &Store, env: &dyn Fn(&str) -> Option<String>) -> CliOutput {
    let mut mode = Mode::FollowUp;
    let mut from = None;
    let mut positional = Vec::new();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--steer" => mode = Mode::Steer,
            "--follow-up" => mode = Mode::FollowUp,
            "--from" => match iter.next() {
                Some(user) => from = Some(user.clone()),
                None => return fail(2, format!("--from needs a value\n{USAGE}")),
            },
            "--" => positional.extend(iter.by_ref().cloned()),
            flag if flag.starts_with("--") => {
                return fail(2, format!("unknown option {flag}\n{USAGE}"));
            }
            _ => positional.push(arg.clone()),
        }
    }
    let Some((target, words)) = positional.split_first() else {
        return fail(2, USAGE.to_owned());
    };
    let message = words.join(" ");
    if message.trim().is_empty() {
        return fail(2, format!("missing message\n{USAGE}"));
    }
    let endpoint = match store.resolve(target, None) {
        Ok(endpoint) => endpoint,
        Err(err) => return fail(1, err.to_string()),
    };
    let user = from
        .or_else(|| env("USER"))
        .or_else(|| env("USERNAME"))
        .filter(|user| !user.trim().is_empty());
    let delivery = Delivery::new(message, Sender::CommandLine { user }, mode, 1);
    match store.deliver(&endpoint.session_id, &delivery) {
        Ok(_) => ok(format!(
            "{} -> {} ({})",
            delivery.delivery_id,
            endpoint.session_id,
            mode.as_str()
        )),
        Err(err) => fail(1, format!("could not write the delivery: {err}")),
    }
}
