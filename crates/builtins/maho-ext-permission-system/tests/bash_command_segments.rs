//! Port of test/permission/bash-command-segments.test.ts and
//! suite/regressions/issue-4309-bash-every-command-segment.test.ts (decision level).
use maho_ext_permission_system::{
    bash_command_segments::{
        BashCommandAnalysis, MAX_COMMAND_LENGTH, analyze_bash_command, best_effort_command_lines,
        last_analysis_steps,
    },
    config::{from_config, rules_for_preset},
    events::PermissionEventEmitter,
    parsers::{PermissionRequest, UNPARSED_BASH_PREFIX, create_builtin_parser_registry},
    service::{AskOptions, PermissionService},
    types::*,
};
use serde_json::json;
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Allow,
    Ask,
    Deny,
}

fn config(entries: &[(&str, &str, &str)]) -> Ruleset {
    let mut grouped: Vec<(String, PermissionValue)> = Vec::new();
    for (permission, pattern, action) in entries {
        let mut map = serde_json::Map::new();
        map.insert((*pattern).into(), json!(action));
        grouped.push(((*permission).into(), PermissionValue::Patterns(map)));
    }
    from_config(&grouped, "/home/user").expect("config")
}

fn parse(command: &str) -> Vec<PermissionRequest> {
    create_builtin_parser_registry().parse(
        "bash",
        &json!({ "command": command }),
        (Path::new("/workspace/project"), Path::new("/home/user")),
    )
}

async fn decide(
    preset: PermissionPresetName,
    command: &str,
    configured: Ruleset,
    approved: Ruleset,
) -> Outcome {
    let mut rules = rules_for_preset(preset);
    rules.extend(configured);
    let mut service = PermissionService::new(rules, approved, PermissionEventEmitter::default());
    for parsed in parse(command) {
        let request = Request::new(
            String::new(),
            "s".into(),
            parsed.permission.clone(),
            parsed.patterns.clone(),
            parsed.always.clone(),
            Default::default(),
        );
        let answer = service.ask_with(
            request,
            AskOptions {
                auto_approve_ask: parsed.auto_approve_ask,
                rule_aliases: None,
            },
        );
        let pending = service.list();
        if !pending.is_empty() {
            for entry in pending {
                service.reply(ReplyInput {
                    request_id: entry.id,
                    reply: Reply::Reject,
                    message: None,
                    always: None,
                });
            }
            let _ = answer.await;
            return Outcome::Ask;
        }
        if answer.await.is_err() {
            return Outcome::Deny;
        }
    }
    Outcome::Allow
}

fn always(prefixes: &[&str]) -> Ruleset {
    prefixes
        .iter()
        .flat_map(|prefix| {
            [
                Rule {
                    permission: "bash".into(),
                    pattern: (*prefix).into(),
                    action: Action::Allow,
                },
                Rule {
                    permission: "bash".into(),
                    pattern: format!("{prefix} *"),
                    action: Action::Allow,
                },
            ]
        })
        .collect()
}

const TARGET: &str = "rm -rf build";

fn chained_shapes() -> Vec<(&'static str, String)> {
    vec![
        ("and-list", format!("cd src && {TARGET}")),
        ("or-list", format!("cd src || {TARGET}")),
        ("sequence", format!("cd src; {TARGET}")),
        ("sequence without space", format!("cd src;{TARGET}")),
        ("pipeline", format!("cd src | {TARGET}")),
        ("pipeline with stderr", format!("cd src |& {TARGET}")),
        ("background job", format!("cd src & {TARGET}")),
        ("line break", format!("cd src\n{TARGET}")),
        ("subshell", format!("cd src; ({TARGET})")),
        ("brace group", format!("cd src; {{ {TARGET}; }}")),
        ("command substitution", format!("cd $({TARGET})")),
        ("quoted command substitution", format!("cd \"$({TARGET})\"")),
        ("backtick substitution", format!("cd `{TARGET}`")),
        ("process substitution", format!("cd <({TARGET})")),
        (
            "here-string substitution",
            format!("cd <<< \"$({TARGET})\""),
        ),
        (
            "here-document substitution",
            format!("cd <<EOF\n$({TARGET})\nEOF"),
        ),
        (
            "parameter default substitution",
            format!("cd ${{dir:-$({TARGET})}}"),
        ),
        (
            "arithmetic substitution",
            format!("cd $(( $({TARGET}) + 1 ))"),
        ),
        ("if body", format!("if cd src; then {TARGET}; fi")),
        (
            "loop body",
            format!("for f in a; do cd src; {TARGET}; done"),
        ),
        (
            "after a redirection",
            format!("cd src 2>/dev/null && {TARGET}"),
        ),
        (
            "after a test",
            format!("[[ -d src && -d out ]] && {TARGET}"),
        ),
        (
            "substitution that starts with a subshell",
            format!("cd $((cd src) ; {TARGET})"),
        ),
    ]
}

fn wrapper_shapes() -> Vec<(&'static str, String)> {
    vec![
        ("env", format!("env FOO=1 {TARGET}")),
        (
            "env with a quoted assignment",
            format!("env 'FOO=1' {TARGET}"),
        ),
        ("su --command", format!("su --command='{TARGET}' root")),
        ("eval --", format!("eval -- '{TARGET}'")),
        ("command", format!("command {TARGET}")),
        ("exec", format!("exec {TARGET}")),
        ("builtin", format!("builtin eval '{TARGET}'")),
        ("nohup", format!("nohup {TARGET}")),
        ("time", format!("time {TARGET}")),
        ("nice", format!("nice -n 5 {TARGET}")),
        ("timeout", format!("timeout 5 {TARGET}")),
        ("xargs", format!("xargs {TARGET}")),
        ("sudo", format!("sudo -u root {TARGET}")),
        ("doas", format!("doas {TARGET}")),
        ("setsid", format!("setsid {TARGET}")),
        ("ionice", format!("ionice -c 3 {TARGET}")),
        ("bash -c", format!("bash -c '{TARGET}'")),
        ("sh -c with flags", format!("sh -ec \"{TARGET}\"")),
        ("eval", format!("eval '{TARGET}'")),
        ("find -exec", format!("find . -exec {TARGET} {{}} \\;")),
    ]
}

const ASKING_PRESETS: [PermissionPresetName; 3] = [
    PermissionPresetName::AcceptEdits,
    PermissionPresetName::Ask,
    PermissionPresetName::ReadOnly,
];

#[tokio::test]
async fn asking_presets_ask_for_every_chained_or_wrapped_command() {
    for preset in ASKING_PRESETS {
        for (shape, command) in chained_shapes() {
            let lead = if command.starts_with("[[") {
                "[["
            } else {
                "cd"
            };
            assert_eq!(
                decide(preset, &command, vec![], always(&[lead])).await,
                Outcome::Ask,
                "{preset:?} {shape}"
            );
        }
        for (shape, command) in wrapper_shapes() {
            let wrapper = command.split(' ').next().expect("wrapper");
            assert_eq!(
                decide(
                    preset,
                    &command,
                    vec![],
                    always(&[wrapper, "builtin", "eval"])
                )
                .await,
                Outcome::Ask,
                "{preset:?} {shape}"
            );
        }
        assert_eq!(
            decide(
                preset,
                &format!("git status && {TARGET}"),
                config(&[("bash", "git *", "allow")]),
                vec![]
            )
            .await,
            Outcome::Ask
        );
        assert_eq!(
            decide(
                preset,
                "cd src && ls -la | wc -l",
                vec![],
                always(&["cd", "ls", "wc"])
            )
            .await,
            Outcome::Allow
        );
        assert_eq!(
            decide(
                preset,
                &format!("cd src; case x in x) {TARGET};; esac"),
                vec![],
                always(&["cd", "case", "rm"])
            )
            .await,
            Outcome::Ask
        );
    }
}

#[tokio::test]
async fn full_access_deny_rule_reaches_every_command() {
    let deny = || config(&[("bash", "rm *", "deny")]);
    for (shape, command) in chained_shapes().into_iter().chain(wrapper_shapes()) {
        assert_eq!(
            decide(PermissionPresetName::FullAccess, &command, deny(), vec![]).await,
            Outcome::Deny,
            "{shape}"
        );
    }
    for command in [
        format!("/bin/{TARGET}"),
        format!("./{TARGET}"),
        "'rm' -rf build".into(),
        format!("LANG=C {TARGET}"),
        format!("LANG=\"C\" {TARGET}"),
    ] {
        assert_eq!(
            decide(PermissionPresetName::FullAccess, &command, deny(), vec![]).await,
            Outcome::Deny,
            "{command}"
        );
    }
    assert_eq!(
        decide(
            PermissionPresetName::FullAccess,
            &format!("case x in x) {TARGET};; esac"),
            deny(),
            vec![]
        )
        .await,
        Outcome::Deny
    );
    assert_eq!(
        decide(
            PermissionPresetName::FullAccess,
            "case x in x) ls;; esac",
            deny(),
            vec![]
        )
        .await,
        Outcome::Allow
    );
}

#[test]
fn bash_permission_requests() {
    let simple = |patterns: &[&str], always: &[&str]| PermissionRequest {
        permission: "bash".into(),
        patterns: patterns.iter().map(|p| (*p).into()).collect(),
        always: always.iter().map(|p| (*p).into()).collect(),
        ..PermissionRequest::default()
    };
    assert_eq!(
        parse("git commit -m \"a b\""),
        vec![simple(&["git commit"], &["git commit", "git commit *"])]
    );
    assert_eq!(
        parse("cd src && ls | cd out"),
        vec![simple(&["cd", "ls"], &["cd", "cd *", "ls", "ls *"])]
    );
    assert_eq!(
        parse("sudo -u root /usr/bin/git push")[0].patterns,
        vec!["sudo", "git push"]
    );
    let command = "case x in x) ls;; esac";
    let target = format!("{UNPARSED_BASH_PREFIX}{command}");
    assert_eq!(
        parse(command),
        vec![
            PermissionRequest {
                auto_approve_ask: true,
                ..simple(&["case", "x", "in", "ls", "esac"], &[])
            },
            simple(&[target.as_str()], &[target.as_str()]),
        ]
    );
}

fn parsed(segments: &[&[&str]]) -> BashCommandAnalysis {
    BashCommandAnalysis::Parsed {
        segments: segments
            .iter()
            .map(|segment| segment.iter().map(|word| (*word).into()).collect())
            .collect(),
    }
}

#[test]
fn analyze_splits_commands() {
    for (command, segments) in [
        ("git status", parsed(&[&["git", "status"]])),
        (
            "a && b || c; d | e & f",
            parsed(&[&["a"], &["b"], &["c"], &["d"], &["e"], &["f"]]),
        ),
        (
            "echo $(a) `b`",
            parsed(&[&["a"], &["b"], &["echo", "", ""]]),
        ),
        ("cat <<'EOF'\n$(a)\nEOF\nb", parsed(&[&["cat"], &["b"]])),
        (
            "cat <<EOF\n$(a)\nEOF\nb",
            parsed(&[&["cat"], &["a"], &["b"]]),
        ),
        ("ls 2>&1 >out", parsed(&[&["ls"]])),
        ("ls # && a", parsed(&[&["ls"]])),
        ("X=1 Y=$(a) b", parsed(&[&["a"], &["b"]])),
        ("[ -f x ] && a", parsed(&[&["[", "-f", "x", "]"], &["a"]])),
        ("echo $((a) ; b)", parsed(&[&["a"], &["b"], &["echo", ""]])),
        ("echo $((1 + 2))", parsed(&[&["echo", ""]])),
        ("ls *.ts", parsed(&[&["ls", "*.ts"]])),
    ] {
        assert_eq!(analyze_bash_command(command), segments, "{command:?}");
    }
}

#[test]
fn analyze_refuses_what_it_does_not_understand() {
    for command in [
        "echo 'a",
        "echo $(a",
        "$cmd x",
        "/bin/r? x",
        "{rm,x} y",
        "case a in a) b;; esac",
        "f() { a; }",
        "sudo --unknown a",
        "env -S 'a b'",
        "bash -c \"$script\"",
    ] {
        assert_eq!(
            analyze_bash_command(command),
            BashCommandAnalysis::Unparsed,
            "{command:?}"
        );
    }
}

#[test]
fn bounded_work_and_fail_closed_limits() {
    let unparsed_script = |words: usize| {
        format!(
            "f() {{ {} }}",
            (0..words)
                .map(|i| format!("echo w{i};"))
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    let candidate_work = |command: &str| {
        best_effort_command_lines(command)
            .iter()
            .map(Vec::len)
            .sum::<usize>() as f64
    };
    let (small, large) = (unparsed_script(500), unparsed_script(1000));
    assert_eq!(analyze_bash_command(&large), BashCommandAnalysis::Unparsed);
    assert!(candidate_work(&large) / candidate_work(&small) < 2.5);

    let chain = |n: usize| {
        (0..n)
            .map(|i| format!("echo \"a$(true)b\" {i}"))
            .collect::<Vec<_>>()
            .join(" && ")
    };
    analyze_bash_command(&chain(500));
    let small = last_analysis_steps() as f64;
    analyze_bash_command(&chain(1000));
    assert!(last_analysis_steps() as f64 / small < 2.5);

    for nested in [
        format!("echo {}1{}", "$((".repeat(5_000), "))".repeat(5_000)),
        format!("echo {}y{}", "${x:-".repeat(5_000), "}".repeat(5_000)),
        format!("echo {}true{}", "$(".repeat(5_000), ")".repeat(5_000)),
    ] {
        assert_eq!(analyze_bash_command(&nested), BashCommandAnalysis::Unparsed);
    }
    // Wrapper chains up to the length limit fail closed instead of exhausting the stack.
    for (unit, tail) in [
        ("env ", "rm -rf build"),
        ("nice ", "rm -rf build"),
        ("eval ", "rm"),
    ] {
        let count = (MAX_COMMAND_LENGTH - tail.len()) / unit.len();
        let deep = format!("{}{tail}", unit.repeat(count));
        assert!(deep.len() <= MAX_COMMAND_LENGTH);
        assert_eq!(
            analyze_bash_command(&deep),
            BashCommandAnalysis::Unparsed,
            "{unit}"
        );
        let requests = parse(&deep);
        assert!(
            requests.iter().any(|request| request
                .patterns
                .iter()
                .any(|pattern| pattern.starts_with(UNPARSED_BASH_PREFIX))),
            "{unit}: {requests:?}"
        );
    }
    // A realistic wrapper chain still parses.
    assert_eq!(
        analyze_bash_command("env A=1 nice -n 5 nohup timeout 5 rm -rf build"),
        parsed(&[
            &["env"],
            &["nice"],
            &["nohup"],
            &["timeout"],
            &["rm", "-rf", "build"]
        ])
    );
    const { assert!(MAX_COMMAND_LENGTH < 100_000) };
    assert_eq!(
        analyze_bash_command(&format!("echo {}", "a".repeat(100_000))),
        BashCommandAnalysis::Unparsed
    );
    assert_eq!(
        analyze_bash_command(&format!("find . \"-exec\" {TARGET} {{}} \\;")),
        parsed(&[
            &["find", ".", "-exec", "rm", "-rf", "build", "{}", ";"],
            &["rm", "-rf", "build", "{}"]
        ])
    );
    assert_eq!(
        analyze_bash_command(&format!("find . $action {TARGET} {{}} \\;")),
        BashCommandAnalysis::Unparsed
    );
}

fn remembered(command: &str) -> Ruleset {
    parse(command)
        .into_iter()
        .flat_map(|request| {
            request.always.into_iter().map(|pattern| Rule {
                permission: "bash".into(),
                pattern,
                action: Action::Allow,
            })
        })
        .collect()
}

#[tokio::test]
async fn allow_always_never_widens() {
    for preset in ASKING_PRESETS {
        let wildcard = remembered("case $1 in *) echo hi;; esac");
        assert!(wildcard.is_empty());
        assert_eq!(
            decide(
                preset,
                &format!("case $1 in x) {TARGET};; *) echo hi;; esac"),
                vec![],
                wildcard
            )
            .await,
            Outcome::Ask
        );
        let exact = remembered("case $1 in a) echo hi;; esac");
        assert_eq!(
            decide(
                preset,
                "case $1 in a) echo hi;; esac",
                vec![],
                exact.clone()
            )
            .await,
            Outcome::Allow
        );
        assert_eq!(
            decide(
                preset,
                &format!("case $1 in a) {TARGET};; esac"),
                vec![],
                exact
            )
            .await,
            Outcome::Ask
        );
    }
}

#[tokio::test]
async fn existing_rules_keep_their_glob_meaning() {
    let edits = PermissionPresetName::AcceptEdits;
    assert_eq!(
        decide(
            edits,
            "npm run t?st",
            config(&[("bash", "npm *", "allow"), ("bash", "npm run t?st", "deny")]),
            vec![]
        )
        .await,
        Outcome::Deny
    );
    assert_eq!(
        decide(
            edits,
            "npm run a%b",
            config(&[("bash", "npm *", "allow"), ("bash", "npm run a%b", "deny")]),
            vec![]
        )
        .await,
        Outcome::Deny
    );
    assert_eq!(
        decide(
            edits,
            "npm run t?st",
            config(&[("bash", "npm run t?st", "allow")]),
            vec![]
        )
        .await,
        Outcome::Allow
    );
    assert_eq!(
        parse("npm run 'x*y' && ls")
            .into_iter()
            .flat_map(|request| request.always)
            .collect::<Vec<_>>(),
        vec!["ls", "ls *"]
    );
    assert_eq!(
        decide(
            edits,
            "npm run 't?st'",
            config(&[("bash", "npm *", "allow"), ("bash", "npm run t?st", "deny")]),
            vec![]
        )
        .await,
        Outcome::Deny
    );
    assert_eq!(
        decide(
            edits,
            "npm run 'x*y'",
            config(&[("bash", "npm *", "allow"), ("bash", "npm run x?y", "deny")]),
            vec![]
        )
        .await,
        Outcome::Deny
    );
    assert_eq!(
        decide(
            edits,
            "npm run 'a%b'",
            config(&[("bash", "npm *", "allow"), ("bash", "npm run a%b", "deny")]),
            vec![]
        )
        .await,
        Outcome::Deny
    );
}

/// senpi#4309: "Allow always" on `cd src` must not allow `cd src && rm -rf build`.
#[tokio::test]
async fn allow_always_on_the_first_command_still_asks_for_a_chain() {
    let approved = remembered("cd src");
    assert_eq!(
        decide(
            PermissionPresetName::AcceptEdits,
            "cd src",
            vec![],
            approved.clone()
        )
        .await,
        Outcome::Allow
    );
    assert_eq!(
        decide(
            PermissionPresetName::AcceptEdits,
            "cd src && rm -rf build",
            vec![],
            approved
        )
        .await,
        Outcome::Ask
    );
}
