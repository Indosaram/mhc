use maho_cli::cli::experimental::{command_options::{parse_transport_address, AuthInput, TransportAddress}, commands::{client::parse_client, server::parse_server}};
fn args(values: &[&str]) -> Vec<String> { values.iter().map(|value| (*value).to_owned()).collect() }
#[test]
fn unix_addresses_decode_paths_and_reject_authorities() {
    assert!(matches!(parse_transport_address("unix:///tmp/test%20socket"), Ok(TransportAddress::Unix(path)) if path == "/tmp/test socket"));
    for value in ["unix://host/tmp/test", "unix:////tmp/test", "unix:///tmp/%00", "unix:///tmp/%zz", "unix:///tmp/test?query"] { assert!(parse_transport_address(value).is_err()); }
}
#[test]
fn radius_requires_lowercase_v4_identity() {
    assert!(parse_transport_address("radius://01234567-89ab-4cde-8fab-0123456789ab").is_ok());
    for value in ["radius://01234567-89ab-5cde-8fab-0123456789ab", "radius://01234567-89AB-4cde-8fab-0123456789ab", "radius://01234567-89ab-4cde-8fab-0123456789ab/path"] { assert!(parse_transport_address(value).is_err()); }
}
#[test]
fn client_accepts_single_prompt_and_repeatable_plugins() {
    let command = parse_client(&args(&["-e", "first", "-e=second", "--model", "model", "--", "--prompt"])).ok().unwrap();
    assert_eq!(command.plugin_packages, args(&["first", "second"]));
    assert_eq!(command.prompt.as_deref(), Some("--prompt"));
}
#[test]
fn client_rejects_conflicting_sessions_and_provider_without_model() {
    let errors = parse_client(&args(&["--session-id", "session", "-c", "--provider", "provider"])).err().unwrap();
    assert_eq!(errors.len(), 2);
}
#[test]
fn client_rejects_multiple_prompts_and_conflicting_auth() {
    assert!(parse_client(&args(&["one", "two"])).is_err());
    assert!(parse_client(&args(&["--auth-token", "test-token", "--auth-token-file", "file"])).is_err());
}
#[test]
fn server_accepts_identifier_and_model_but_not_prompts() {
    let command = parse_server(&args(&["--server-id", "01234567-89ab-4cde-8fab-0123456789ab", "--model", "model", "--session-dir", "/tmp/sessions"])).ok().unwrap();
    assert_eq!(command.session_dir.as_deref(), Some("/tmp/sessions"));
    assert!(parse_server(&args(&["--server-id", "invalid"])).is_err());
    assert!(parse_server(&args(&["prompt"])).is_err());
}

// Upstream packages/coding-agent/test/experimental-cli-command.test.ts (senpi pin 903b8336):
// every case driven through the parse_server/parse_client entry points.
const UNSUPPORTED_SERVER_OPTIONS: &str = "The experimental server command does not support existing CLI options yet";
const UNSUPPORTED_CLIENT_OPTIONS: &str = "The experimental client command does not support existing CLI options yet";
const SERVER_ID: &str = "00000000-0000-4000-8000-000000000001";
#[test]
fn upstream_parses_server_configuration() {
    let command = parse_server(&args(&["--server-id", SERVER_ID, "--session-dir", "~/pi-sessions", "--provider", "anthropic", "--model", "claude-sonnet-4-5", "-e", "./first-plugin", "-e=./second-plugin"])).ok().unwrap();
    assert_eq!(command.server_id.as_deref(), Some(SERVER_ID));
    assert_eq!(command.session_dir.as_deref(), Some("~/pi-sessions"));
    assert_eq!(command.provider.as_deref(), Some("anthropic"));
    assert_eq!(command.model.as_deref(), Some("claude-sonnet-4-5"));
    assert_eq!(command.plugin_packages, args(&["./first-plugin", "./second-plugin"]));
    assert!(command.auth.is_none());
}
#[test]
fn upstream_parses_provider_qualified_server_models() {
    let command = parse_server(&args(&["--model", "anthropic/claude-sonnet-4-5:high"])).ok().unwrap();
    assert_eq!(command.model.as_deref(), Some("anthropic/claude-sonnet-4-5:high"));
    assert!(command.provider.is_none() && command.server_id.is_none() && command.session_dir.is_none() && command.auth.is_none() && command.plugin_packages.is_empty());
}
#[test]
fn upstream_parses_client_transport_addresses() {
    let command = parse_client(&args(&["--connect", "unix:///tmp/pi.sock"])).ok().unwrap();
    assert!(matches!(command.connect, Some(TransportAddress::Unix(ref path)) if path == "/tmp/pi.sock"));
    assert!(command.session_id.is_none());
    let command = parse_client(&args(&["--connect", "radius://00000000-0000-4000-8000-000000000001", "--session-id", "demo-1"])).ok().unwrap();
    assert!(matches!(command.connect, Some(TransportAddress::Radius(ref id)) if id == SERVER_ID));
    assert_eq!(command.session_id.as_deref(), Some("demo-1"));
}
#[test]
fn upstream_parses_client_session_selection() {
    for (option, resume) in [("-c", false), ("--continue", false), ("-r", true), ("--resume", true)] {
        let command = parse_client(&args(&[option])).ok().unwrap();
        assert_eq!((command.continue_session, command.resume), (!resume, resume), "{option}");
        assert!(command.prompt.is_none() && command.session_id.is_none() && command.connect.is_none(), "{option}");
    }
}
#[test]
fn upstream_parses_session_selection_followed_by_one_shot_prompt() {
    let command = parse_client(&args(&["-r", "Explain this project"])).ok().unwrap();
    assert!(command.resume && !command.continue_session);
    assert_eq!(command.prompt.as_deref(), Some("Explain this project"));
}
#[test]
fn upstream_parses_client_model_and_plugin_configuration() {
    let command = parse_client(&args(&["--model", "anthropic/claude-sonnet-4-5:high", "-e", "./first-plugin", "-e", "./second-plugin"])).ok().unwrap();
    assert_eq!(command.model.as_deref(), Some("anthropic/claude-sonnet-4-5:high"));
    assert_eq!(command.plugin_packages, args(&["./first-plugin", "./second-plugin"]));
    assert!(command.provider.is_none() && command.prompt.is_none());
}
#[test]
fn upstream_parses_authentication_sources() {
    let cases = [(["--auth-token", "secret"], AuthInput::Token("secret".to_owned())), (["--auth-token-file", "/tmp/token"], AuthInput::File("/tmp/token".to_owned()))];
    for (argv, auth) in cases {
        assert!(parse_server(&args(&argv)).ok().unwrap().auth == Some(auth.clone()), "server {argv:?}");
        assert!(parse_client(&args(&argv)).ok().unwrap().auth == Some(auth), "client {argv:?}");
    }
}
#[test]
fn upstream_permits_omitted_authentication() {
    let server = parse_server(&[]).ok().unwrap();
    assert!(server.auth.is_none() && server.provider.is_none() && server.model.is_none() && server.server_id.is_none() && server.session_dir.is_none() && server.plugin_packages.is_empty());
    let client = parse_client(&[]).ok().unwrap();
    assert!(client.auth.is_none() && client.connect.is_none() && client.session_id.is_none() && !client.continue_session && !client.resume && client.provider.is_none() && client.model.is_none() && client.plugin_packages.is_empty() && client.prompt.is_none());
}
#[test]
fn upstream_rejects_invalid_experimental_input() {
    let cases: [(&[&str], &str); 17] = [
        (&["client", "--listen", "unix:///tmp/pi.sock"], UNSUPPORTED_CLIENT_OPTIONS),
        (&["server", "--listen", "unix:///tmp/pi.sock"], UNSUPPORTED_SERVER_OPTIONS),
        (&["server", "--connect", "unix:///tmp/pi.sock"], UNSUPPORTED_SERVER_OPTIONS),
        (&["client", "--connect", "ws://localhost:8080"], "Unsupported --connect transport \"ws:\""),
        (&["client", "--connect", "radius://not-a-server"], "Radius transport address requires"),
        (&["client", "--connect", "unix://relative.sock"], "Unix transport address must not include an authority"),
        (&["client", "--connect", "unix:///tmp/pi.sock?wrong=value"], "Invalid --connect address"),
        (&["client", "--provider", "anthropic"], "--provider requires --model"),
        (&["client", "-c", "-r"], "--session-id, --continue, and --resume are mutually exclusive"),
        (&["client", "--continue=true"], "--continue does not take a value"),
        (&["server", "--provider", "anthropic"], "--provider requires --model"),
        (&["server", "--server-id", "not-a-uuid"], "Invalid --server-id"),
        (&["server", "--server-id"], "--server-id requires a value"),
        (&["server", "--session-dir"], "--session-dir requires a value"),
        (&["client", "-e"], "-e requires a value"),
        (&["server", "--session-dir", "/tmp/first", "--session-dir=/tmp/second"], "--session-dir may only be specified once"),
        (&["client", "--connect="], "--connect requires a value"),
    ];
    for (argv, error) in cases {
        let rest = args(&argv[1..]);
        let errors = match argv[0] { "server" => parse_server(&rest).err(), _ => parse_client(&rest).err() }.unwrap_or_else(|| panic!("{argv:?} parsed"));
        assert!(errors.iter().any(|message| message.contains(error)), "{argv:?}: {errors:?} lacks {error:?}");
    }
}
#[test]
fn upstream_rejects_unsupported_options_without_parsing_them_through_the_stable_cli() {
    assert_eq!(parse_client(&args(&["--tui-mode", "wrong", "--model", "claude-sonnet"])).err().unwrap(), vec![UNSUPPORTED_CLIENT_OPTIONS.to_owned()]);
}

// senpi client.ts/server.ts `.action((command, context) => context.runClient/runServer(command))`:
// executing the built command hands the parsed command to the context; a rejected argv never does.
#[derive(Default)]
struct RecordingContext { servers: std::sync::Mutex<Vec<Option<String>>>, clients: std::sync::Mutex<Vec<Option<String>>> }
impl maho_cli::cli::experimental::commands::server::ServerCommandContext for RecordingContext {
    fn run_server(&self, command: maho_cli::cli::experimental::commands::server::ServerCommand) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>> {
        self.servers.lock().expect("recorded").push(command.model);
        Box::pin(async { Ok(()) })
    }
}
impl maho_cli::cli::experimental::commands::client::ClientCommandContext for RecordingContext {
    fn run_client(&self, command: maho_cli::cli::experimental::commands::client::ClientCommand) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>> {
        self.clients.lock().expect("recorded").push(command.prompt);
        Box::pin(async { Ok(()) })
    }
}
#[tokio::test]
async fn executing_the_commands_hands_the_built_command_to_the_context_action() {
    use maho_cli::cli::experimental::commands::{client::client_command, server::server_command};
    let context = std::sync::Arc::new(RecordingContext::default());
    let server = server_command().expect("server command");
    let served = server.execute(&args(&["--provider", "openai", "--model", "gpt-5"]), context.clone()).await.expect("action");
    assert!(served.is_ok());
    let refused = server.execute(&args(&["--provider", "openai"]), context.clone()).await.expect("no action error");
    assert_eq!(refused.err(), Some(vec!["--provider requires --model".to_owned()]));
    assert_eq!(*context.servers.lock().unwrap(), vec![Some("gpt-5".to_owned())]);
    let client = client_command().expect("client command");
    assert!(client.execute(&args(&["--", "hello"]), context.clone()).await.expect("action").is_ok());
    assert!(client.execute(&args(&["--continue", "--resume"]), context.clone()).await.expect("no action error").is_err());
    assert_eq!(*context.clients.lock().unwrap(), vec![Some("hello".to_owned())]);
}

/// senpi `cli/experimental/cli.ts`: the `experimental` group dispatches `server`/`client` to their parsers and answers
/// anything else with the group's own error.
#[test]
fn experimental_group_dispatches_server_and_client_and_rejects_others() {
    use maho_cli::cli::experimental::cli::{parse, ExperimentalInvocation};
    assert!(matches!(parse(&args(&["client", "-e", "hi"])), Ok(ExperimentalInvocation::Client(_))));
    assert!(matches!(parse(&args(&["server"])), Ok(ExperimentalInvocation::Server(_))));
    for argv in [&[][..], &["bogus"][..]] {
        assert_eq!(parse(&args(argv)).err(), Some(vec!["Expected experimental command: server or client".to_owned()]));
    }
    assert_eq!(parse(&args(&["client", "one", "two"])).is_err(), parse_client(&args(&["one", "two"])).is_err());
}
