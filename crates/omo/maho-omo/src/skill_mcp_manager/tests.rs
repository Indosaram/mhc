use super::{env_grants_store as store, manager::*, project_env_policy as policy, types::*};
use serde_json::json;
use std::sync::Arc;
fn env(entries: &[(&str, &str)]) -> Environment {
    entries
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}
fn label() -> store::GrantLabel {
    store::GrantLabel {
        project: "/synthetic/project".into(),
        skill: "carrier".into(),
        server: "probe".into(),
    }
}
fn info() -> SkillMcpClientInfo {
    SkillMcpClientInfo {
        session_id: "review".into(),
        skill_name: "fixture".into(),
        server_name: "fixture".into(),
        scope: Some("project".into()),
        directory: Some("/synthetic/project".into()),
        source_path: Some("/synthetic/project/SKILL.md".into()),
        ..Default::default()
    }
}
// The shared upstream fixture hashes POSIX path spellings; Windows resolves `/fixture/...` against
// the current drive, so the digest is only comparable on Unix.
#[cfg(unix)]
#[test]
fn declaration_identity_shared_fixture() {
    let identity = json!({"sourcePath":"/fixture/project/skills/a/mcp.json","sourceKind":"sidecar","projectRoot":"/fixture/project","serverName":"probe","raw":{"type":"stdio","command":"tool","args":["--serve"],"cwd":"./work","env":{"TOKEN":"${TOKEN}","NODE_OPTIONS":"--no-warnings"}},"cwdBase":"/fixture/project"});
    let expected = "a82aa53bdc6ae462ccbe72166353100b4dbf4fc6d07dfea01d12dc9c1f8f3c63";
    assert_eq!(policy::policy_hash(&identity), expected);
    let declaration = SkillMcpClientInfo {
        server_name: "probe".into(),
        source_path: Some("/fixture/project/skills/a/mcp.json".into()),
        source_kind: Some("sidecar".into()),
        directory: Some("/fixture/project".into()),
        ..Default::default()
    };
    assert_eq!(
        policy::declaration_digest(&declaration, &identity["raw"]),
        expected
    );
    for field in ["command", "env", "url", "sourceKind", "cwdBase"] {
        let mut changed = identity.clone();
        if matches!(field, "sourceKind" | "cwdBase") {
            changed[field] = json!("other");
        } else {
            changed["raw"][field] = json!("other");
        }
        assert_ne!(policy::policy_hash(&changed), expected);
    }
}
#[test]
fn windows_safe_base_and_declared_override() {
    let ambient = env(&[
        ("Path", "lower-path"),
        ("PATH", "upper-path"),
        ("LC_PRIVATE_LOCALE", "synthetic-private"),
        ("HTTPS_PROXY", "synthetic-proxy"),
    ]);
    let inherited = policy::resolve_project_environment(
        &json!({"command":"synthetic"}),
        &[],
        &Environment::new(),
        &ambient,
        true,
    );
    assert_eq!(
        inherited.env.get("PATH").map(String::as_str),
        Some("upper-path")
    );
    assert!(!inherited.env.contains_key("Path"));
    let declared = policy::resolve_project_environment(
        &json!({"command":"synthetic","env":{"Path":"declared-path","NPM_CONFIG_EXPLICIT":"explicit-value"}}),
        &[],
        &Environment::new(),
        &ambient,
        true,
    );
    assert_eq!(
        declared.env.get("Path").map(String::as_str),
        Some("declared-path")
    );
    assert!(!declared.env.contains_key("PATH"));
    assert_eq!(
        declared.env.get("NPM_CONFIG_EXPLICIT").map(String::as_str),
        Some("explicit-value")
    );
    assert_eq!(inherited.withheld, vec!["HTTPS_PROXY", "LC_PRIVATE_LOCALE"]);
}
#[test]
fn safe_granted_explicit_declared_order() {
    let result = policy::resolve_project_environment(
        &json!({"command":"synthetic","env":{"OPAQUE":"declared-opaque","PNPM_PRIVATE":"declared-package"}}),
        &["OPAQUE".into(), "PNPM_PRIVATE".into()],
        &env(&[("HOME", "caller-home"), ("OPAQUE", "caller-opaque")]),
        &env(&[
            ("HOME", "ambient-home"),
            ("OPAQUE", "ambient-opaque"),
            ("PNPM_PRIVATE", "ambient-package"),
            ("LC_PRIVATE_LOCALE", "ambient-locale"),
        ]),
        false,
    );
    assert_eq!(
        result.env,
        env(&[
            ("HOME", "caller-home"),
            ("OPAQUE", "declared-opaque"),
            ("PNPM_PRIVATE", "declared-package")
        ])
    );
    assert_eq!(result.withheld, vec!["LC_PRIVATE_LOCALE"]);
}
#[test]
fn expansion_keeps_withheld_literals_and_never_values_in_notice_set() {
    let raw = json!({"url":"https://example.invalid/${PRIVATE_TOKEN}","headers":{"Authorization":"${PRIVATE_TOKEN:-fallback}"},"oauth":{"clientId":"${PRIVATE_ID}","scopes":["${HOME}"]}});
    let ambient = env(&[
        ("PRIVATE_TOKEN", "secret"),
        ("PRIVATE_ID", "secret-id"),
        ("HOME", "home"),
    ]);
    let result =
        policy::resolve_project_environment(&raw, &[], &Environment::new(), &ambient, false);
    assert_eq!(result.config["url"], raw["url"]);
    assert_eq!(result.withheld, vec!["PRIVATE_ID", "PRIVATE_TOKEN"]);
    let result = policy::resolve_project_environment(
        &raw,
        &["PRIVATE_TOKEN".into()],
        &Environment::new(),
        &ambient,
        false,
    );
    assert_eq!(result.config["url"], "https://example.invalid/secret");
    assert_eq!(result.withheld, vec!["PRIVATE_ID"]);
}
#[test]
fn clean_ambient_secrets_but_preserve_declared_and_client_keys() {
    assert_eq!(
        policy::create_clean_mcp_environment(
            &env(&[("OPENAI_API_KEY", "declared-secret"), ("SAFE_FLAG", "1")]),
            &env(&[("OPENAI_API_KEY", "ambient-secret")])
        ),
        env(&[("OPENAI_API_KEY", "declared-secret"), ("SAFE_FLAG", "1")])
    );
    let info = SkillMcpClientInfo {
        session_id: "ses_1".into(),
        skill_name: "skill-a".into(),
        server_name: "server-a".into(),
        ..Default::default()
    };
    assert_eq!(
        build_skill_mcp_client_key(&info, None),
        "ses_1:skill-a:server-a"
    );
    assert_eq!(
        build_skill_mcp_client_key(
            &info,
            Some(&SkillMcpClientOptions {
                cdp_url: Some("ws://127.0.0.1/devtools".into()),
                ..Default::default()
            })
        ),
        "ses_1:skill-a:server-a::cdp=ws://127.0.0.1/devtools"
    );
}
#[test]
fn concurrent_writers_merge_and_revoke_selected_names() {
    let root = tempfile::tempdir().expect("root");
    let path = store::resolve_grants_store_path(Some(root.path()));
    let first = store::declaration_digest(&json!({"command":"one"}));
    let second = store::declaration_digest(&json!({"command":"two"}));
    std::thread::scope(|scope| {
        for digest in [&first, &second] {
            let path = &path;
            scope.spawn(move || {
                store::add_grant(path, digest, label(), &["Z".into(), "A".into(), "A".into()])
                    .expect("grant");
            });
        }
    });
    assert_eq!(store::read(&path).generation, 2);
    assert_eq!(store::read(&path).grants[&first].names, vec!["A", "Z"]);
    store::revoke(&path, &first, Some(&["A".into()])).expect("revoke subset");
    assert_eq!(store::read(&path).grants[&first].names, vec!["Z"]);
    store::revoke(&path, &first, None).expect("revoke");
    assert_eq!(
        store::list(&path).keys().cloned().collect::<Vec<_>>(),
        vec![second]
    );
}
#[tokio::test]
async fn file_store_adapter_preserves_shared_schema_and_digest() {
    use super::env_grant_store::create_file_grant_store;
    let root = tempfile::tempdir().expect("root");
    let path = store::resolve_grants_store_path(Some(root.path()));
    assert_eq!(path, root.path().join("mcp-env-grants.json"));
    let adapter = create_file_grant_store(Some(root.path()));
    let digest = "a".repeat(64);
    adapter
        .add(
            &digest,
            store::Grant {
                label: label(),
                names: vec!["Z".into(), "A".into(), "A".into()],
            },
        )
        .expect("adapter add");
    store::add_grant(&path, &digest, label(), &["B".into()]).expect("shared add");
    let snapshot = adapter.read();
    assert_eq!(snapshot.snapshot.generation, 2);
    assert_eq!(adapter.list()[&digest].names, ["A", "B", "Z"]);
    assert_eq!(
        adapter.digest(),
        store::declaration_digest(&serde_json::to_value(store::read(&path)).expect("snapshot"))
    );
    let wire: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("file")).expect("JSON");
    assert_eq!(
        wire,
        json!({"version":1,"generation":2,"grants":{(digest.clone()):{"label":{"project":"/synthetic/project","skill":"carrier","server":"probe"},"names":["A","B","Z"]}}})
    );
    adapter.revoke(&digest).expect("adapter revoke");
    assert!(store::read(&path).grants.is_empty());
}
#[test]
fn valid_non_identifier_names_and_invalid_input_fail_closed() {
    let root = tempfile::tempdir().expect("root");
    let path = store::resolve_grants_store_path(Some(root.path()));
    let identity = "a".repeat(64);
    store::add_grant(
        &path,
        &identity,
        label(),
        &["ProgramFiles(x86)".into(), "odd-name".into()],
    )
    .expect("grant");
    for name in ["A=B", "A\0B", ""] {
        assert!(store::add_grant(&path, &identity, label(), &[name.into()]).is_err());
    }
    assert_eq!(store::read(&path).generation, 1);
    for text in [
        "not-json",
        r#"{"version":2,"generation":1,"grants":{}}"#,
        r#"{"version":1,"generation":-1,"grants":{}}"#,
    ] {
        std::fs::write(&path, text).expect("corrupt");
        assert!(store::read(&path).grants.is_empty());
    }
}
#[tokio::test]
async fn pagination_collects_pages_and_bounds_infinite_cursor() {
    let mut page = 0;
    let result = collect_pages(|cursor| {
        page += 1;
        assert_eq!(cursor, if page == 1 { None } else { Some("next".into()) });
        std::future::ready(Ok((
            vec![page],
            if page < 3 { Some("next".into()) } else { None },
        )))
    })
    .await
    .expect("pages");
    assert_eq!(result, vec![1, 2, 3]);
    let mut requests = 0;
    assert!(
        collect_pages(|_| {
            requests += 1;
            std::future::ready(Ok((vec![()], Some("again".into()))))
        })
        .await
        .is_err()
    );
    assert_eq!(requests, 100);
}
struct UnwatchedStore(super::env_grant_store::FileGrantStore);
impl super::env_grant_store::GrantStore for UnwatchedStore {
    fn read(&self) -> super::env_grant_store::EnvGrantSnapshot {
        super::env_grant_store::GrantStore::read(&self.0)
    }
    fn add(&self, digest: &str, grant: store::Grant) -> super::Result<()> {
        super::env_grant_store::GrantStore::add(&self.0, digest, grant)
    }
    fn revoke(&self, digest: &str) -> super::Result<()> {
        super::env_grant_store::GrantStore::revoke(&self.0, digest)
    }
    fn watch(
        &self,
        _listener: Box<dyn Fn(store::GrantObservation) + Send>,
    ) -> Option<store::Observation> {
        None
    }
}
fn grant_fixture() -> (
    Arc<super::environment_grant_policy::EnvironmentGrantPolicy>,
    tempfile::TempDir,
    Arc<std::sync::Mutex<Vec<String>>>,
) {
    let root = tempfile::tempdir().expect("root");
    let ids = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = ids.clone();
    let policy = super::environment_grant_policy::EnvironmentGrantPolicy::new(
        Arc::new(UnwatchedStore(super::env_grant_store::FileGrantStore {
            path: store::resolve_grants_store_path(Some(root.path())),
        })),
        Some(Arc::new(move |message, _| {
            let id = message
                .split("/omo-mcp-env-grant ")
                .nth(1)
                .expect("handle")
                .split_whitespace()
                .next()
                .expect("id");
            captured.lock().expect("ids").push(id.into());
        })),
        Arc::new(|_| Box::pin(async {})),
    );
    (policy, root, ids)
}
#[tokio::test]
async fn superseded_notices_expire_and_pending_handles_are_bounded() {
    let (policy, _root, ids) = grant_fixture();
    let info = info();
    let raw = json!({"type":"http","url":"https://example.invalid/${REVIEW_PRIVATE_TOKEN}"});
    policy.prepare(&info, &raw, &Environment::new()).await;
    let first = ids.lock().expect("ids")[0].clone();
    policy
        .prepare(&info, &raw, &env(&[("REVIEW_EXPLICIT", "changed")]))
        .await;
    assert!(policy.grant(&first, None).await.is_err());
    assert_eq!(ids.lock().expect("ids").len(), 2);
    let second = ids.lock().expect("ids")[1].clone();
    assert!(
        policy
            .grant(&second, Some(&["NOT_DISPLAYED".into()]))
            .await
            .is_err()
    );
    policy.grant(&second, None).await.expect("grant");
    for i in 0..129 {
        let mut info = info.clone();
        info.server_name = format!("server-{i}");
        policy.prepare(&info, &raw, &Environment::new()).await;
    }
    let first_server = ids.lock().expect("ids")[2].clone();
    assert!(policy.grant(&first_server, None).await.is_err());
    let count = ids.lock().expect("ids").len();
    let mut info = info;
    info.server_name = "server-0".into();
    policy.prepare(&info, &raw, &Environment::new()).await;
    assert_eq!(ids.lock().expect("ids").len(), count + 1);
    policy.close().await;
}
#[test]
fn project_scope_and_provenance_determine_environment_authority() {
    for scope in ["project", "opencode-project", "local"] {
        let info = SkillMcpClientInfo {
            scope: Some(scope.into()),
            ..Default::default()
        };
        assert!(policy::uses_project_environment(&info));
    }
    let mut info = SkillMcpClientInfo {
        scope: Some("host".into()),
        ..Default::default()
    };
    assert!(policy::uses_project_environment(&info));
    info.host_user_trusted = true;
    assert!(!policy::uses_project_environment(&info));
    info.provenance = Some(Provenance {
        kind: "project-auto".into(),
        selected_root: "/project".into(),
        project_root: Some("/project".into()),
    });
    assert!(policy::uses_project_environment(&info));
    for kind in ["user", "builtin"] {
        info.scope = Some("project".into());
        info.provenance.as_mut().expect("provenance").kind = kind.into();
        assert!(!policy::uses_project_environment(&info));
    }
}
#[tokio::test]
async fn changed_or_removed_declaration_rejects_the_grant_without_persistence() {
    for removed in [false, true] {
        let (policy, root, ids) = grant_fixture();
        let mut info = info();
        info.revalidate_declaration = Some(Arc::new(move || {
            Box::pin(async move {
                Ok(if removed {
                    None
                } else {
                    Some(Declaration {
                        config: json!({"url":"https://changed.invalid"}),
                        source_path: "/synthetic/project/SKILL.md".into(),
                        source_kind: "frontmatter".into(),
                    })
                })
            })
        }));
        policy
            .prepare(
                &info,
                &json!({"url":"https://fixture.invalid/${REVIEW_PRIVATE_TOKEN}"}),
                &Environment::new(),
            )
            .await;
        let id = ids.lock().expect("ids")[0].clone();
        assert!(
            policy
                .grant(&id, None)
                .await
                .expect_err("stale grant")
                .contains("declaration changed")
        );
        assert!(
            store::read(&store::resolve_grants_store_path(Some(root.path())))
                .grants
                .is_empty()
        );
    }
}
#[tokio::test]
async fn supersession_during_revalidation_never_writes() {
    let (policy, _root, ids) = grant_fixture();
    let raw = json!({"type":"http","url":"https://example.invalid/${REVIEW_PRIVATE_TOKEN}"});
    let mut info = info();
    let (entered, mut started) = tokio::sync::mpsc::unbounded_channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let resume = release.clone();
    let declaration_raw = raw.clone();
    info.revalidate_declaration = Some(Arc::new(move || {
        let entered = entered.clone();
        let release = resume.clone();
        let raw = declaration_raw.clone();
        Box::pin(async move {
            let wait = release.notified();
            tokio::pin!(wait);
            wait.as_mut().enable();
            entered.send(()).expect("entered");
            wait.await;
            Ok(Some(Declaration {
                config: raw,
                source_path: "/synthetic/project/SKILL.md".into(),
                source_kind: "frontmatter".into(),
            }))
        })
    }));
    policy.prepare(&info, &raw, &Environment::new()).await;
    let id = ids.lock().expect("ids")[0].clone();
    let granting = tokio::spawn({
        let policy = policy.clone();
        async move { policy.grant(&id, None).await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), started.recv())
        .await
        .expect("revalidation entered")
        .expect("event");
    policy
        .prepare(&info, &raw, &env(&[("REVIEW_EXPLICIT", "changed")]))
        .await;
    release.notify_waiters();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), granting)
            .await
            .expect("grant completed")
            .expect("task")
            .is_err()
    );
    policy.close().await;
}
#[tokio::test]
async fn revocation_aborts_started_policy_and_unrelated_grants_do_not() {
    let (policy, root, _ids) = grant_fixture();
    let info = info();
    let raw = json!({"command":"probe"});
    let first = policy
        .prepare(&info, &raw, &Environment::new())
        .await
        .expect("project policy");
    let path = store::resolve_grants_store_path(Some(root.path()));
    store::add_grant(&path, &"b".repeat(64), label(), &["OTHER".into()]).expect("unrelated");
    policy.refresh(false).await;
    assert_eq!(policy.version(&first.identity).await, first.version);
    assert!(!*first.signal.borrow());
    store::add_grant(&path, &first.identity, label(), &["PRIVATE".into()]).expect("grant");
    policy.refresh(false).await;
    assert!(*first.signal.borrow());
    assert_eq!(policy.version(&first.identity).await, first.version + 1);
    policy.close().await;
}

#[tokio::test]
async fn observes_creation_atomic_replacement_and_conservative_gap() {
    let root = tempfile::tempdir().expect("root");
    let path = store::resolve_grants_store_path(Some(&root.path().join("nested")));
    let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
    let _observation = store::observe(path.clone(), move |event| {
        sender.send(event).expect("observation receiver");
    });
    let initial = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
        .await
        .expect("initial observation")
        .expect("event");
    assert_eq!(initial.snapshot.generation, 0);
    let digest = "a".repeat(64);
    store::add_grant(&path, &digest, label(), &["ODD_DSN".into()]).expect("grant");
    let created = tokio::time::timeout(std::time::Duration::from_secs(7), async {
        loop {
            let event = events.recv().await.expect("event");
            if event.snapshot.generation == 1 {
                break event;
            }
        }
    })
    .await
    .expect("creation event");
    assert_eq!(created.snapshot.grants[&digest].names, vec!["ODD_DSN"]);
    store::revoke(&path, &digest, None).expect("revoke");
    let revoked = tokio::time::timeout(std::time::Duration::from_secs(7), async {
        loop {
            let event = events.recv().await.expect("event");
            if event.snapshot.generation == 2 {
                break event;
            }
        }
    })
    .await
    .expect("revoke event");
    assert!(revoked.snapshot.grants.is_empty());
    let mut snapshot = store::read(&path);
    snapshot.generation = 9;
    let replacement = path.with_extension("replacement");
    std::fs::write(&replacement, serde_json::to_vec(&snapshot).expect("json")).expect("write");
    std::fs::rename(replacement, &path).expect("replace");
    let gap = tokio::time::timeout(std::time::Duration::from_secs(7), async {
        loop {
            let event = events.recv().await.expect("event");
            if event.snapshot.generation == 9 {
                break event;
            }
        }
    })
    .await
    .expect("gap event");
    assert!(gap.conservative);
}
#[tokio::test]
async fn policy_abort_wins_and_signal_changes_cancel_pending_connection() {
    use super::policy_connection::await_policy_signal;
    let (controller, signal) = tokio::sync::watch::channel(false);
    let (entered, started) = tokio::sync::oneshot::channel();
    let connecting = tokio::spawn(async move {
        await_policy_signal(
            async {
                entered.send(()).expect("entered");
                std::future::pending::<super::Result<()>>().await
            },
            Some(signal),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), started)
        .await
        .expect("entered in time")
        .expect("entered");
    controller.send_replace(true);
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(2), connecting)
            .await
            .expect("cancelled")
            .expect("task")
            .is_err()
    );
}

#[test]
fn separate_process_grant_writer() {
    let Some(path) = std::env::var_os("MHC_GRANT_TEST_PATH") else {
        return;
    };
    let digest = std::env::var("MHC_GRANT_TEST_DIGEST").expect("digest");
    if std::env::var_os("MHC_GRANT_TEST_REVOKE").is_some() {
        store::revoke(std::path::Path::new(&path), &digest, None).expect("child revoke");
    } else {
        store::add_grant(
            std::path::Path::new(&path),
            &digest,
            label(),
            &["ODD_DSN".into()],
        )
        .expect("child grant");
    }
}
#[tokio::test]
async fn separate_atomic_writers_converge() {
    let root = tempfile::tempdir().expect("root");
    let path = store::resolve_grants_store_path(Some(root.path()));
    let executable = std::env::current_exe().expect("test binary");
    let mut children = Vec::new();
    for digest in ["a".repeat(64), "b".repeat(64)] {
        children.push(
            tokio::process::Command::new(&executable)
                .args([
                    "--exact",
                    "skill_mcp_manager::tests::separate_process_grant_writer",
                    "--nocapture",
                ])
                .env("MHC_GRANT_TEST_PATH", &path)
                .env("MHC_GRANT_TEST_DIGEST", digest)
                .kill_on_drop(true)
                .spawn()
                .expect("child"),
        );
    }
    for mut child in children {
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
                .await
                .expect("writer exit timeout")
                .expect("child exit")
                .success()
        );
    }
    let snapshot = store::read(&path);
    assert_eq!(snapshot.generation, 2);
    assert_eq!(snapshot.grants.len(), 2);
    assert_eq!(
        std::fs::read_dir(root.path()).expect("directory").count(),
        1
    );
}

#[tokio::test]
async fn real_http_manager_deduplicates_lists_pages_and_reconnects_after_reload() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let address = listener.local_addr().expect("address");
    let initializations = Arc::new(AtomicUsize::new(0));
    let (pending_started, mut pending_events) = tokio::sync::mpsc::unbounded_channel();
    let count = initializations.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let count = count.clone();
            let pending_started = pending_started.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let header_end;
                loop {
                    let mut chunk = [0u8; 1024];
                    let read = stream.read(&mut chunk).await.expect("headers");
                    if read == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&chunk[..read]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        header_end = end + 4;
                        break;
                    }
                }
                let headers = String::from_utf8_lossy(&bytes[..header_end]);
                if headers.starts_with("GET ") {
                    stream.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.expect("GET response");
                    return;
                }
                if headers.starts_with("DELETE ") {
                    stream.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.expect("DELETE response");
                    return;
                }
                let mode = headers
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(name, _)| name.eq_ignore_ascii_case("x-fixture-mode"))
                            .map(|(_, value)| value.trim().to_owned())
                    })
                    .unwrap_or_else(|| "paged".into());
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                            .map(|(_, v)| v.trim().parse::<usize>().expect("length"))
                    })
                    .unwrap_or(0);
                while bytes.len() < header_end + length {
                    let mut chunk = [0u8; 1024];
                    let read = stream.read(&mut chunk).await.expect("body");
                    assert!(read > 0);
                    bytes.extend_from_slice(&chunk[..read]);
                }
                let request: serde_json::Value =
                    serde_json::from_slice(&bytes[header_end..header_end + length])
                        .expect("request JSON");
                if request.get("id").is_none() {
                    stream.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.expect("notification");
                    return;
                }
                if request["method"] == "initialize" && mode == "pending" {
                    pending_started.send(()).expect("pending signal");
                    // The real client cancellation closes this socket; no timing delay is needed.
                    let mut byte = [0];
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(15),
                        stream.read(&mut byte),
                    )
                    .await
                    .expect("cancelled socket timeout")
                    .expect("cancelled socket");
                    return;
                }
                let result = match request["method"].as_str().expect("method") {
                    "initialize" => {
                        count.fetch_add(1, Ordering::SeqCst);
                        json!({"protocolVersion":"2025-11-25","capabilities":{},"serverInfo":{"name":"fixture","version":"1"}})
                    }
                    method @ ("tools/list" | "resources/list" | "prompts/list") => {
                        let catalog = method.split('/').next().expect("catalog");
                        let page = request["params"]["cursor"]
                            .as_str()
                            .map(|cursor| cursor.parse::<usize>().expect("opaque cursor"))
                            .unwrap_or(1);
                        let item = match catalog {
                            "tools" => {
                                json!({"name":format!("item-{page}"),"inputSchema":{"type":"object"}})
                            }
                            "resources" => {
                                json!({"name":format!("item-{page}"),"uri":format!("fixture://item-{page}")})
                            }
                            _ => json!({"name":format!("item-{page}")}),
                        };
                        let mut result = if mode == "single" {
                            json!({(catalog):[]})
                        } else {
                            json!({(catalog):[item]})
                        };
                        if mode == "endless" || (mode == "paged" && page < 2) {
                            result["nextCursor"] = json!((page + 1).to_string());
                        }
                        result
                    }
                    "tools/call" => {
                        json!({"content":[{"type":"text","text":"tool failed"}],"isError":true,"structuredContent":{"preserved":true},"_meta":{"fixture":"kept"}})
                    }
                    _ => panic!("unexpected request: {request}"),
                };
                let body = json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string();
                stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).as_bytes()).await.expect("response");
            });
        }
    });
    let root = tempfile::tempdir().expect("root");
    let manager = SkillMcpManager::new(Some(root.path()), None, None);
    let info = SkillMcpClientInfo {
        session_id: "session".into(),
        skill_name: "fixture".into(),
        server_name: "fixture".into(),
        scope: Some("user".into()),
        ..Default::default()
    };
    let config = json!({"url":format!("http://{address}/mcp")});
    let (first, second) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(
            manager.get_or_create_client(&info, &config, None),
            manager.get_or_create_client(&info, &config, None)
        )
    })
    .await
    .expect("connections");
    assert!(Arc::ptr_eq(
        &first.expect("first"),
        &second.expect("second")
    ));
    assert_eq!(initializations.load(Ordering::SeqCst), 1);
    let context = SkillMcpServerContext {
        config: config.clone(),
        skill_name: "fixture".into(),
    };
    let tools = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        manager.list_tools(&info, &context, None),
    )
    .await
    .expect("list timeout")
    .expect("tools");
    assert_eq!(
        tools
            .iter()
            .map(|t| t["name"].as_str().expect("name"))
            .collect::<Vec<_>>(),
        vec!["item-1", "item-2"]
    );
    for catalog in ["resources", "prompts"] {
        let items = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            if catalog == "resources" {
                manager.list_resources(&info, &context, None).await
            } else {
                manager.list_prompts(&info, &context, None).await
            }
        })
        .await
        .expect("catalog timeout")
        .expect("catalog");
        assert_eq!(
            items
                .iter()
                .map(|item| item["name"].as_str().expect("name"))
                .collect::<Vec<_>>(),
            ["item-1", "item-2"]
        );
    }
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        manager.call_tool(&info, &context, "failure", json!({}), None),
    )
    .await
    .expect("call timeout")
    .expect("tool result");
    assert_eq!(
        result,
        json!({"content":[{"type":"text","text":"tool failed"}],"isError":true,"structuredContent":{"preserved":true},"_meta":{"fixture":"kept"}})
    );
    for mode in ["single", "endless"] {
        let config =
            json!({"url":format!("http://{address}/mcp"),"headers":{"x-fixture-mode":mode}});
        let catalog_info = SkillMcpClientInfo {
            session_id: mode.into(),
            ..info.clone()
        };
        let catalog_context = SkillMcpServerContext {
            config,
            skill_name: "fixture".into(),
        };
        for catalog in ["tools", "resources", "prompts"] {
            let result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                match catalog {
                    "tools" => {
                        manager
                            .list_tools(&catalog_info, &catalog_context, None)
                            .await
                    }
                    "resources" => {
                        manager
                            .list_resources(&catalog_info, &catalog_context, None)
                            .await
                    }
                    _ => {
                        manager
                            .list_prompts(&catalog_info, &catalog_context, None)
                            .await
                    }
                }
            })
            .await
            .expect("catalog timeout");
            if mode == "single" {
                assert!(result.expect("empty catalog").is_empty());
            } else {
                assert!(result.expect_err("endless catalog").contains("100 pages"));
            }
        }
    }
    manager.disconnect_all().await;
    assert!(!manager.is_connected(&info).await);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        manager.get_or_create_client(&info, &config, None),
    )
    .await
    .expect("reconnect timeout")
    .expect("reconnect");
    assert_eq!(initializations.load(Ordering::SeqCst), 4);
    manager.disconnect_session("session").await;
    assert!(!manager.is_connected(&info).await);
    let project_info = SkillMcpClientInfo {
        session_id: "project".into(),
        scope: Some("project".into()),
        directory: Some(root.path().to_string_lossy().into_owned()),
        ..info.clone()
    };
    let project_config =
        json!({"url":format!("http://{address}/mcp"),"headers":{"x-private":"${ODD_DSN}"}});
    let identity = policy::declaration_digest(&project_info, &project_config);
    let path = store::resolve_grants_store_path(Some(root.path()));
    store::add_grant(&path, &identity, label(), &["ODD_DSN".into()])
        .expect("initial project grant");
    let live = manager
        .get_or_create_client(&project_info, &project_config, None)
        .await
        .expect("live project");
    let pending_config = json!({"url":format!("http://{address}/mcp"),"headers":{"x-fixture-mode":"pending","x-private":"${ODD_DSN}"}});
    let pending_identity = policy::declaration_digest(&project_info, &pending_config);
    store::add_grant(&path, &pending_identity, label(), &["ODD_DSN".into()])
        .expect("pending project grant");
    let connecting = manager.get_or_create_client(&project_info, &pending_config, None);
    tokio::pin!(connecting);
    tokio::select! {
        result = &mut connecting => panic!("pending initialization completed: {}", result.is_ok()),
        event = tokio::time::timeout(std::time::Duration::from_secs(5), pending_events.recv()) => { event.expect("initialization event timeout").expect("event"); }
    }
    for digest in [&identity, &pending_identity] {
        let status = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            tokio::process::Command::new(std::env::current_exe().expect("test binary"))
                .args([
                    "--exact",
                    "skill_mcp_manager::tests::separate_process_grant_writer",
                    "--nocapture",
                ])
                .env("MHC_GRANT_TEST_PATH", &path)
                .env("MHC_GRANT_TEST_DIGEST", digest)
                .env("MHC_GRANT_TEST_REVOKE", "1")
                .kill_on_drop(true)
                .status(),
        )
        .await
        .expect("revoke child timeout")
        .expect("revoke child");
        assert!(status.success());
    }
    let error = tokio::time::timeout(std::time::Duration::from_secs(8), &mut connecting)
        .await
        .expect("pending connection cancelled")
        .err()
        .expect("policy rejection");
    assert!(error.contains("policy changed"), "{error}");
    manager.environment_policy.refresh(false).await;
    let renewed = manager
        .get_or_create_client(&project_info, &project_config, None)
        .await
        .expect("renewed project");
    assert!(!Arc::ptr_eq(&live, &renewed));
    assert!(store::read(&path).grants.is_empty());
    manager.disconnect_all().await;
    server.abort();
    assert!(server.await.expect_err("aborted").is_cancelled());
}
