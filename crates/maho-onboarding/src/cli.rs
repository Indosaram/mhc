//! `mhc setup` (spec §3): argument parsing, the non-interactive path and the interactive flow over
//! a caller-supplied prompter.

use crate::flow::{self, Prompter, Stop};
use crate::host::{Choice, SetupHost, apply, resolve_model, resolve_provider};
use crate::steps::{ALL_STEPS, CURRENT_SETUP_VERSION, SelectOptions, select};
use serde_json::json;

pub const USAGE: &str = "Usage: mhc setup [--force]
       mhc setup --provider <id> [--api-key-env <VAR> | --api-key-stdin] [--model <provider/id | id>] [--yes] [--json]
       mhc setup --check

Without --provider, asks for a provider, an API key (when none is stored) and a default model on
the terminal. With --provider it runs without prompts (--yes is required when stdin is not a
terminal). --check prints the stored and current setup versions.";

/// Parsed `mhc setup` arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub api_key_env: Option<String>,
    pub api_key_stdin: bool,
    pub yes: bool,
    pub json: bool,
    pub force: bool,
    pub check: bool,
    pub help: bool,
}

/// Parses `mhc setup` arguments.
pub fn parse(args: &[String]) -> Result<Args, String> {
    let mut out = Args::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = |name: &str| {
            iter.next()
                .cloned()
                .ok_or_else(|| format!("{name} requires a value"))
        };
        match arg.as_str() {
            "--provider" => out.provider = Some(value("--provider")?),
            "--model" => out.model = Some(value("--model")?),
            "--api-key-env" => out.api_key_env = Some(value("--api-key-env")?),
            "--api-key-stdin" => out.api_key_stdin = true,
            "--yes" | "-y" => out.yes = true,
            "--json" => out.json = true,
            "--force" => out.force = true,
            "--check" => out.check = true,
            "--help" | "-h" => out.help = true,
            other => return Err(format!("unknown argument \"{other}\"")),
        }
    }
    if out.api_key_env.is_some() && out.api_key_stdin {
        return Err("--api-key-env and --api-key-stdin are exclusive".into());
    }
    if out.provider.is_none() && (out.api_key_env.is_some() || out.api_key_stdin) {
        return Err("an API key option needs --provider".into());
    }
    Ok(out)
}

/// Output of one command run.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

fn usage_error(message: &str) -> Output {
    Output {
        stderr: format!("mhc setup: {message}\n{USAGE}"),
        code: 2,
        ..Output::default()
    }
}

fn failure(message: &str) -> Output {
    Output {
        stderr: format!("mhc setup: {message}"),
        code: 1,
        ..Output::default()
    }
}

/// The process facts `mhc setup` depends on.
pub struct Io<'a> {
    pub stdin_tty: bool,
    pub stdout_tty: bool,
    pub stored_version: u32,
    pub env: &'a dyn Fn(&str) -> Option<String>,
    /// Reads the whole of stdin (for `--api-key-stdin`).
    pub read_stdin: &'a mut dyn FnMut() -> Result<String, String>,
}

/// Runs `mhc setup <args>`. `prompter` is used by the interactive path only.
pub fn run(
    args: &[String],
    host: &mut dyn SetupHost,
    prompter: &mut dyn Prompter,
    io: Io<'_>,
) -> Output {
    let args = match parse(args) {
        Ok(args) => args,
        Err(message) => return usage_error(&message),
    };
    if args.help {
        return Output {
            stdout: USAGE.to_owned(),
            ..Output::default()
        };
    }
    if args.check {
        let would_run = !select(
            io.stored_version,
            &SelectOptions {
                is_tty: true,
                ..SelectOptions::default()
            },
        )
        .is_empty();
        return Output {
            stdout: if args.json {
                json!({"stored_version": io.stored_version, "current_version": CURRENT_SETUP_VERSION, "would_run": would_run}).to_string()
            } else {
                format!(
                    "stored setup version: {}\ncurrent setup version: {CURRENT_SETUP_VERSION}\nsetup would run: {}",
                    io.stored_version,
                    if would_run { "yes" } else { "no" }
                )
            },
            ..Output::default()
        };
    }
    match args.provider.clone() {
        Some(provider) => non_interactive(&args, &provider, host, io),
        None => {
            if !(io.stdin_tty && io.stdout_tty) {
                return failure(
                    "interactive setup needs a terminal; use --provider <id> [--model ...] --yes",
                );
            }
            let steps = if args.force {
                ALL_STEPS.to_vec()
            } else {
                // `mhc setup` is an explicit request: every step runs, as with `force`.
                select(
                    0,
                    &SelectOptions {
                        is_tty: true,
                        ..SelectOptions::default()
                    },
                )
            };
            interactive(host, prompter, &steps, None)
        }
    }
}

/// Runs the prompts and applies the result.
pub fn interactive(
    host: &mut dyn SetupHost,
    prompter: &mut dyn Prompter,
    steps: &[crate::steps::Step],
    preset_provider: Option<&str>,
) -> Output {
    match flow::run(host, prompter, steps, preset_provider) {
        Ok(choice) => match apply(host, &choice, CURRENT_SETUP_VERSION) {
            Ok(()) => Output {
                stdout: format!("setup complete: {}/{}", choice.provider, choice.model),
                ..Output::default()
            },
            Err(error) => failure(&error),
        },
        Err(Stop::Aborted) => failure("cancelled; nothing was changed"),
        Err(Stop::Empty(message)) => failure(&message),
    }
}

fn non_interactive(args: &Args, provider: &str, host: &mut dyn SetupHost, io: Io<'_>) -> Output {
    if !io.stdin_tty && !args.yes {
        return usage_error("--yes is required when stdin is not a terminal");
    }
    let providers = host.providers();
    let Some(provider) = resolve_provider(&providers, provider).cloned() else {
        return usage_error(&format!("unknown provider \"{provider}\""));
    };
    let models = host.models(&provider.id);
    let model = match args.model.as_deref() {
        Some(answer) => match resolve_model(&provider.id, &models, answer) {
            Some(model) => model,
            None => {
                return usage_error(&format!(
                    "unknown model \"{answer}\" for provider {}",
                    provider.id
                ));
            }
        },
        None => match models.first() {
            Some(model) => model.clone(),
            None => return failure(&format!("provider {} has no models", provider.id)),
        },
    };
    let key = if let Some(var) = &args.api_key_env {
        match (io.env)(var).filter(|v| !v.trim().is_empty()) {
            Some(key) => Some(key),
            None => return usage_error(&format!("environment variable {var} is empty or unset")),
        }
    } else if args.api_key_stdin {
        match (io.read_stdin)() {
            Ok(text) if !text.trim().is_empty() => Some(text.trim().to_owned()),
            Ok(_) => return usage_error("no API key on stdin"),
            Err(error) => return failure(&error),
        }
    } else {
        None
    };
    let choice = Choice {
        provider: provider.id.clone(),
        model,
        key,
    };
    if let Err(error) = apply(host, &choice, CURRENT_SETUP_VERSION) {
        return failure(&error);
    }
    let stdout = if args.json {
        json!({
            "provider": choice.provider,
            "model": format!("{}/{}", choice.provider, choice.model),
            "stored_key": choice.key.is_some(),
            "version": CURRENT_SETUP_VERSION,
        })
        .to_string()
    } else {
        format!("setup complete: {}/{}", choice.provider, choice.model)
    };
    Output {
        stdout,
        ..Output::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::Scripted;
    use crate::host::fake::FakeHost;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn go(list: &[&str], host: &mut FakeHost, tty: bool, answers: &[&str]) -> Output {
        let env = |key: &str| (key == "KEY_VAR").then(|| "sk-env".to_owned());
        let mut stdin = || Ok("sk-stdin\n".to_owned());
        run(
            &args(list),
            host,
            &mut Scripted::new(answers),
            Io {
                stdin_tty: tty,
                stdout_tty: tty,
                stored_version: 0,
                env: &env,
                read_stdin: &mut stdin,
            },
        )
    }

    #[test]
    fn non_interactive_applies_provider_key_and_model() {
        let mut host = FakeHost::sample();
        let out = go(
            &[
                "--provider",
                "beta",
                "--api-key-env",
                "KEY_VAR",
                "--yes",
                "--json",
            ],
            &mut host,
            false,
            &[],
        );
        assert_eq!(out.code, 0, "{out:?}");
        let value: serde_json::Value = serde_json::from_str(&out.stdout).expect("json");
        assert_eq!(value["model"], "beta/b1");
        assert_eq!(value["stored_key"], true);
        assert_eq!(
            host.log,
            ["key beta sk-env", "model beta/b1", "done 1 beta/b1"]
        );
    }

    #[test]
    fn stdin_key_and_qualified_model() {
        let mut host = FakeHost::sample();
        let out = go(
            &[
                "--provider",
                "alpha",
                "--api-key-stdin",
                "--model",
                "alpha/a2",
                "-y",
            ],
            &mut host,
            false,
            &[],
        );
        assert_eq!(out.stdout, "setup complete: alpha/a2");
        assert_eq!(host.log[0], "key alpha sk-stdin");
    }

    #[test]
    fn validation_writes_nothing() {
        for list in [
            &["--provider", "beta"][..],
            &["--provider", "gamma", "--yes"][..],
            &["--provider", "beta", "--model", "zz", "--yes"][..],
            &["--provider", "beta", "--api-key-env", "NOPE", "--yes"][..],
            &["--api-key-stdin"][..],
            &["--bogus"][..],
        ] {
            let mut host = FakeHost::sample();
            let out = go(list, &mut host, false, &[]);
            assert_eq!(out.code, 2, "{list:?} {out:?}");
            assert!(host.log.is_empty(), "{list:?}");
        }
    }

    #[test]
    fn interactive_needs_a_terminal_and_eof_cancels() {
        let mut host = FakeHost::sample();
        assert_eq!(go(&[], &mut host, false, &[]).code, 1);
        let out = go(&[], &mut host, true, &["beta"]);
        assert_eq!(out.code, 1);
        assert!(out.stderr.contains("nothing was changed"));
        assert!(host.log.is_empty());
        let out = go(&[], &mut host, true, &["1", "a2"]);
        assert_eq!(out.stdout, "setup complete: alpha/a2");
        assert_eq!(host.log, ["model alpha/a2", "done 1 alpha/a2"]);
    }

    #[test]
    fn check_reports_versions() {
        let mut host = FakeHost::sample();
        let out = go(&["--check"], &mut host, false, &[]);
        assert!(
            out.stdout.contains("setup would run: yes"),
            "{}",
            out.stdout
        );
        assert_eq!(go(&["--help"], &mut host, false, &[]).code, 0);
    }
}
