use maho_server::app_server::cli_args::{CliArgs, DaemonVerb, Listen, WsAuth, parse_cli_args};
fn parse(args: &[&str]) -> CliArgs {
    parse_cli_args(&args.iter().map(|s| (*s).into()).collect::<Vec<_>>())
}
#[test]
fn default_server_and_daemon_transport_and_flags() {
    assert_eq!(
        parse(&[]),
        CliArgs::Server {
            listen: Listen::Stdio {
                url: "stdio://".into()
            },
            ws_auth: None,
            json_logs: false
        }
    );
    assert_eq!(
        parse(&["daemon", "status"]),
        CliArgs::Daemon {
            verb: DaemonVerb::Status,
            listen: Listen::Ws {
                url: "ws://127.0.0.1:18800".into(),
                host: "127.0.0.1".into(),
                port: 18800
            }
        }
    );
    assert_eq!(
        parse(&[
            "--listen",
            "unix:///tmp/a.sock",
            "--ws-auth",
            "off",
            "--json-logs"
        ]),
        CliArgs::Server {
            listen: Listen::Unix {
                url: "unix:///tmp/a.sock".into(),
                path: Some("/tmp/a.sock".into())
            },
            ws_auth: Some(WsAuth::Off),
            json_logs: true
        }
    );
}
#[test]
fn listen_validation_preserves_whatwg_normalization() {
    for invalid in [
        "ws://localhost:18800",
        "ws://127.0.0.1:80",
        "ws://127.0.0.1:0",
        "ws://[::1]:18800",
        "ws://127.0.0.1:18800/path",
        "ws://user@127.0.0.1:18800",
        "ws://127.0.0.1:18800?q=x",
        "unix://relative",
    ] {
        assert!(
            matches!(parse(&["--listen", invalid]), CliArgs::UsageError { .. }),
            "{invalid}"
        );
    }
    assert!(
        matches!(parse(&["--listen","ws://127.1:18800"]),CliArgs::Server {listen:Listen::Ws {host,..},..} if host=="127.0.0.1")
    );
    assert!(matches!(
        parse(&["--listen", "ws://127.0.0.1:18800?#"]),
        CliArgs::Server { .. }
    ));
}
#[test]
fn repeated_options_and_daemon_arguments() {
    assert!(matches!(
        parse(&["--listen", "unix://", "--listen", "stdio://"]),
        CliArgs::Server {
            listen: Listen::Stdio { .. },
            ..
        }
    ));
    for invalid in [
        vec!["--listen"],
        vec!["--ws-auth"],
        vec!["daemon"],
        vec!["daemon", "start", "--json-logs"],
        vec!["unknown"],
    ] {
        assert!(matches!(parse(&invalid), CliArgs::UsageError { .. }));
    }
    assert!(
        matches!(parse(&["--ws-auth","token.txt"]),CliArgs::Server {ws_auth:Some(WsAuth::TokenFile {path}),..} if path=="token.txt")
    );
}
