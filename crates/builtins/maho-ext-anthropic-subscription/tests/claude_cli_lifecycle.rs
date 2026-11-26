use crate::fixture_credential_store::DiskStore;
use maho_ai::{
    auth::types::{
        AuthEvent, AuthInteraction, AuthPrompt, AuthPromptKind, Credential, OAuthAuth,
        ProviderAuthInteraction,
    },
    utils::abort::{AbortController, AbortSignal},
};
use maho_ext_anthropic_subscription::{
    account_management::{
        add_provider_account, get_provider_accounts, remove_provider_account_with_cli,
    },
    accounts::{SENTINEL_TOKEN, list_accounts},
    executable::{ResolveInput, describe},
    oauth_login::{AnthropicSubscriptionOAuth, ClaudeCliLoginContext},
    provider_id::ANTHROPIC_SUBSCRIPTION_PROVIDER_ID as PROVIDER,
};
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, path::Path, sync::Arc};

struct LoginUi {
    signal: AbortSignal,
    name: &'static str,
    copy_code: bool,
    abort_on_probe: Option<AbortController>,
    abort_on_paste: Option<AbortController>,
}

#[async_trait::async_trait]
impl AuthInteraction for LoginUi {
    fn signal(&self) -> Option<AbortSignal> {
        Some(self.signal.clone())
    }

    async fn prompt(&self, prompt: AuthPrompt) -> anyhow::Result<String> {
        let answer = match prompt.kind {
            AuthPromptKind::Text { .. } => self.name,
            AuthPromptKind::Select { options, .. } => {
                let index = usize::from(self.copy_code);
                return Ok(options[index].id.clone());
            }
            AuthPromptKind::ManualCode { placeholder, .. } => {
                assert_eq!(
                    placeholder.as_deref(),
                    Some("https://claude.ai/oauth/authorize?fixture=manual")
                );
                if let Some(controller) = &self.abort_on_paste {
                    controller.abort(None);
                    anyhow::bail!("cancelled at the code prompt");
                }
                "fixture-code"
            }
            AuthPromptKind::Secret { .. } => anyhow::bail!("unexpected secret prompt"),
        };
        Ok(answer.into())
    }

    fn notify(&self, event: AuthEvent) {
        assert!(
            !self.copy_code || !matches!(&event, AuthEvent::AuthUrl { .. }),
            "copy-code mode must not dispatch a browser-opening event"
        );
        if let Some(controller) = &self.abort_on_probe
            && matches!(event, AuthEvent::Progress { message } if message.starts_with("Testing the account"))
        {
            controller.abort(None);
        }
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    oauth: AnthropicSubscriptionOAuth,
}

impl Fixture {
    fn new(consent: bool) -> Self {
        let temp = tempfile::tempdir().expect("fixture home");
        let root = temp.path();
        let agent_dir = root.join("agent");
        std::fs::create_dir(&agent_dir).expect("agent directory");
        assert_eq!(
            maho_core::config::claude_cli_account_root(&agent_dir),
            agent_dir.join("anthropic-subscription-claude"),
            "the fixture must not inherit a host account root"
        );
        if consent {
            let mut settings = maho_core::settings_manager::SettingsManager::create(
                root.to_str().expect("cwd"),
                agent_dir.to_str().expect("agent directory"),
                root.to_str().expect("home"),
                false,
            );
            maho_core::provider_consent::record_consent(&mut settings, PROVIDER)
                .expect("fixture consent");
        }
        let auth = agent_dir.join("auth.json");
        let executable = root.join("fake-claude");
        std::fs::write(
            &executable,
            include_str!("fixtures/claude_cli_lifecycle.sh"),
        )
        .expect("fake Claude");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
            .expect("executable");
        let environment: std::collections::BTreeMap<String, String> = [
            ("HOME".into(), root.to_string_lossy().into_owned()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            (
                "CLAUDE_CONFIG_DIR".into(),
                root.join(".claude").to_string_lossy().into_owned(),
            ),
            (
                "CLAUDE_CODE_EXECUTABLE".into(),
                executable.to_string_lossy().into_owned(),
            ),
            ("FIXTURE_AUTH".into(), auth.to_string_lossy().into_owned()),
        ]
        .into();
        let override_path = std::path::PathBuf::from(&environment["CLAUDE_CODE_EXECUTABLE"]);
        let resolve = Arc::new(move || {
            let found = describe(
                ResolveInput {
                    platform: std::env::consts::OS,
                    arch: std::env::consts::ARCH,
                    prefer_musl: false,
                    override_path: Some(&override_path),
                    path: None,
                },
                |_| None,
                None,
                None,
            )?;
            Ok((
                found
                    .executable
                    .ok_or_else(|| anyhow::anyhow!("fixture override missing"))?,
                found.source,
            ))
        });
        let store = Arc::new(DiskStore::new(&auth));
        let oauth = AnthropicSubscriptionOAuth::native_with_context(
            store,
            Arc::new(Default::default),
            ClaudeCliLoginContext {
                agent_dir,
                cwd: root.into(),
                environment,
                resolve,
            },
        );
        Self { _temp: temp, oauth }
    }

    fn interaction(
        name: &'static str,
        copy_code: bool,
        cancel_probe: bool,
    ) -> ProviderAuthInteraction {
        let controller = AbortController::new();
        let signal = controller.signal();
        ProviderAuthInteraction::new(
            signal.clone(),
            Arc::new(LoginUi {
                signal,
                name,
                copy_code,
                abort_on_probe: cancel_probe.then_some(controller),
                abort_on_paste: None,
            }),
        )
    }

    async fn login(&self, name: &'static str, copy_code: bool) {
        if copy_code {
            std::fs::write(self.oauth.cli.cwd.join("use-manual"), "").expect("manual-code mode");
        }
        add_provider_account(
            &self.oauth,
            &Self::interaction(name, copy_code, false),
            &self.oauth.cli.agent_dir,
        )
        .await
        .expect("fixture login");
    }

    fn auth(&self) -> Value {
        let path = self.oauth.cli.agent_dir.join("auth.json");
        serde_json::from_slice(&std::fs::read(path).expect("auth.json")).expect("saved credential")
    }

    fn dir(&self, id: &str) -> std::path::PathBuf {
        maho_ext_anthropic_subscription::claude_cli_account_dir::account_dir_for(
            &self.oauth.cli.agent_dir,
            id,
        )
        .expect("managed path")
    }

    fn logs(&self) -> String {
        std::fs::read_to_string(self.oauth.cli.cwd.join("logged-out")).expect("logout receipt")
    }
}

#[tokio::test]
async fn add_account_commits_cli_slot_before_the_connection_probe() {
    let fixture = Fixture::new(true);
    fixture.login("", true).await;
    let auth = fixture.auth();
    let slot = &auth[PROVIDER]["accounts"][0];
    assert_eq!(auth[PROVIDER]["access"], SENTINEL_TOKEN);
    assert_eq!(slot["name"], "default");
    assert_eq!(slot["store"], "claude-cli");
    assert!(slot.get("access").is_none() && slot.get("refresh").is_none());
    assert_eq!(slot["profile"]["subscriptionType"], "max");
    assert_eq!(slot["check"]["outcome"], "ok");
    assert!(
        fixture
            .dir(slot["dirId"].as_str().expect("dir ID"))
            .join("login")
            .is_file()
    );
}

#[tokio::test]
async fn list_accounts_returns_both_cli_slots_and_the_default_name() {
    let fixture = Fixture::new(true);
    fixture.login("", false).await;
    fixture.login("work", false).await;
    let list = get_provider_accounts(
        fixture.oauth.store.as_ref(),
        PROVIDER,
        &fixture.oauth.cli.environment,
        0.0,
    )
    .await
    .expect("account summaries");
    assert_eq!(
        list.iter()
            .map(|slot| slot.name.as_str())
            .collect::<Vec<_>>(),
        ["default", "work"]
    );
}

#[tokio::test]
async fn blank_account_name_skips_the_existing_generated_name() {
    let fixture = Fixture::new(true);
    fixture.login("", false).await;
    fixture.login("account-3", false).await;
    fixture.login("", false).await;
    let auth = fixture.auth();
    let names: Vec<_> = auth[PROVIDER]["accounts"]
        .as_array()
        .expect("accounts")
        .iter()
        .map(|slot| slot["name"].as_str().expect("name"))
        .collect();
    assert_eq!(names, ["default", "account-3", "account-4"]);
}

#[tokio::test]
async fn failed_connection_probe_keeps_the_account_without_a_block() {
    let fixture = Fixture::new(true);
    std::fs::write(fixture.oauth.cli.cwd.join("reject-probe"), "").expect("reject probe fixture");
    fixture.login("", false).await;
    let auth = fixture.auth();
    let slot = &auth[PROVIDER]["accounts"][0];
    assert_eq!(slot["check"]["outcome"], "failed");
    assert_eq!(slot["check"]["status"], 401);
    assert!(slot.get("blockedUntil").is_none());
    assert!(
        fixture
            .dir(slot["dirId"].as_str().expect("dir ID"))
            .join("login")
            .is_file()
    );
}

#[tokio::test]
async fn remove_account_signs_out_its_dir_and_removes_only_its_slot() {
    let fixture = Fixture::new(true);
    fixture.login("", false).await;
    fixture.login("work", false).await;
    let before = fixture.auth();
    let id = before[PROVIDER]["accounts"][0]["dirId"]
        .as_str()
        .expect("dir ID");
    let result = remove_provider_account_with_cli(&fixture.oauth, "default", None)
        .await
        .expect("remove");
    assert!(result.notice.is_none());
    assert_eq!(fixture.logs().trim_end(), id);
    assert!(!fixture.dir(id).exists());
    assert_eq!(
        fixture.auth()[PROVIDER]["accounts"],
        json!([before[PROVIDER]["accounts"][1]])
    );
}

#[tokio::test]
async fn removing_an_account_does_not_hold_the_store_lock_during_cli_logout() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let fixture = Fixture::new(true);
    fixture.login("", false).await;
    fixture.login("work", false).await;
    let listener = tokio::net::UnixListener::bind(fixture.oauth.cli.cwd.join("logout-gate.sock"))
        .expect("logout gate");
    let removal = remove_provider_account_with_cli(&fixture.oauth, "default", None);
    let concurrent = async {
        let (mut stream, _) =
            tokio::time::timeout(std::time::Duration::from_secs(10), listener.accept())
                .await
                .expect("logout reached gate")
                .expect("accept logout");
        let mut ready = [0; 1];
        stream.read_exact(&mut ready).await.expect("logout ready");
        assert_eq!(ready, *b"R");
        let mutation = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            fixture.oauth.store.modify(
                PROVIDER,
                Box::new(|credential| {
                    Box::pin(async move {
                        let mut credential = credential
                            .and_then(Credential::into_oauth)
                            .expect("account pool");
                        let accounts = credential.extra["accounts"]
                            .as_array_mut()
                            .expect("accounts");
                        let work = accounts
                            .iter_mut()
                            .find(|slot| slot["name"] == "work")
                            .expect("work slot");
                        work["displayName"] = json!("concurrent");
                        Ok(Some(Credential::OAuth(credential)))
                    })
                }),
                None,
            ),
        )
        .await;
        stream.write_all(b"G").await.expect("release logout");
        mutation
            .expect("another mutation completed during logout")
            .expect("save sibling");
    };
    let (removed, ()) = tokio::join!(removal, concurrent);
    removed.expect("remove");
    assert_eq!(fixture.auth()[PROVIDER]["accounts"][0]["name"], "work");
    assert_eq!(
        fixture.auth()[PROVIDER]["accounts"][0]["displayName"],
        "concurrent"
    );
}

#[tokio::test]
async fn removing_an_account_does_not_delete_a_concurrent_replacement() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let fixture = Fixture::new(true);
    fixture.login("", false).await;
    let replacement = "0123456789abcdef0123456789abcdef";
    let replacement_dir = fixture.dir(replacement);
    std::fs::create_dir_all(&replacement_dir).expect("replacement account");
    std::fs::write(replacement_dir.join("login"), "").expect("replacement login");
    let listener = tokio::net::UnixListener::bind(fixture.oauth.cli.cwd.join("logout-gate.sock"))
        .expect("logout gate");
    let removal = remove_provider_account_with_cli(&fixture.oauth, "default", None);
    let concurrent = async {
        let (mut stream, _) =
            tokio::time::timeout(std::time::Duration::from_secs(10), listener.accept())
                .await
                .expect("logout reached gate")
                .expect("accept logout");
        let mut ready = [0; 1];
        stream.read_exact(&mut ready).await.expect("logout ready");
        assert_eq!(ready, *b"R");
        let mutation = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            fixture.oauth.store.modify(
                PROVIDER,
                Box::new(move |credential| {
                    Box::pin(async move {
                        let mut credential = credential
                            .and_then(Credential::into_oauth)
                            .expect("account pool");
                        credential.extra["accounts"][0]["dirId"] = json!(replacement);
                        Ok(Some(Credential::OAuth(credential)))
                    })
                }),
                None,
            ),
        )
        .await;
        stream.write_all(b"G").await.expect("release logout");
        mutation
            .expect("replacement completed during logout")
            .expect("save replacement");
    };
    let (removed, ()) = tokio::join!(removal, concurrent);
    assert!(removed.is_err());
    assert_eq!(
        fixture.auth()[PROVIDER]["accounts"][0]["dirId"],
        replacement
    );
    assert!(replacement_dir.join("login").exists());
}

#[tokio::test]
async fn reconnect_retires_old_directory_and_keeps_pin_order_and_name() {
    let fixture = Fixture::new(true);
    fixture.login("", false).await;
    let before = fixture.auth();
    let id = before[PROVIDER]["accounts"][0]["dirId"]
        .as_str()
        .expect("dir ID");
    fixture
        .oauth
        .store
        .modify(
            PROVIDER,
            Box::new(|credential| {
                Box::pin(async move {
                    let mut credential = credential
                        .and_then(Credential::into_oauth)
                        .expect("account pool");
                    credential.extra.insert("pinned".into(), json!("default"));
                    credential.extra.insert("order".into(), json!(["default"]));
                    credential.extra["accounts"][0]["displayName"] = json!("Work");
                    Ok(Some(Credential::OAuth(credential)))
                })
            }),
            None,
        )
        .await
        .expect("pin and order");
    fixture.login("default", false).await;
    let after = fixture.auth();
    let slot = &after[PROVIDER]["accounts"][0];
    assert_eq!(slot["name"], "default");
    assert_eq!(slot["displayName"], "Work");
    assert_ne!(slot["dirId"], id);
    assert_eq!(after[PROVIDER]["pinned"], "default");
    assert_eq!(after[PROVIDER]["order"], json!(["default"]));
    assert_eq!(fixture.logs().trim_end(), id);
    assert!(!fixture.dir(id).exists());
}

#[tokio::test]
async fn cancel_after_slot_write_rolls_back_reconnect_without_touching_old_login() {
    let fixture = Fixture::new(true);
    fixture.login("", false).await;
    let before = fixture.auth();
    let old_id = before[PROVIDER]["accounts"][0]["dirId"]
        .as_str()
        .expect("dir ID");
    assert!(
        fixture
            .oauth
            .login(&Fixture::interaction("default", false, true))
            .await
            .is_err()
    );
    assert_eq!(fixture.auth(), before);
    let retired = fixture.logs();
    assert_ne!(retired.trim_end(), old_id);
    assert!(!fixture.dir(retired.trim_end()).exists());
    assert!(fixture.dir(old_id).join("login").exists());
}

#[tokio::test]
async fn cancel_mid_login_signs_out_a_saved_but_uncommitted_cli_login() {
    let fixture = Fixture::new(true);
    for marker in ["use-manual", "save-before-code"] {
        std::fs::write(fixture.oauth.cli.cwd.join(marker), "").expect("login fixture mode");
    }
    let controller = AbortController::new();
    let signal = controller.signal();
    let interaction = ProviderAuthInteraction::new(
        signal.clone(),
        Arc::new(LoginUi {
            signal,
            name: "",
            copy_code: true,
            abort_on_probe: None,
            abort_on_paste: Some(controller),
        }),
    );
    assert!(fixture.oauth.login(&interaction).await.is_err());
    let retired = fixture.logs();
    assert!(!fixture.dir(retired.trim_end()).exists());
    assert!(
        fixture
            .oauth
            .store
            .read(PROVIDER, None)
            .await
            .expect("store read")
            .is_none()
    );
}

#[tokio::test]
async fn a_rejected_auth_status_signs_out_and_never_commits_the_slot() {
    let fixture = Fixture::new(true);
    std::fs::write(fixture.oauth.cli.cwd.join("reject-status"), "").expect("reject status fixture");
    assert!(
        fixture
            .oauth
            .login(&Fixture::interaction("", false, false))
            .await
            .is_err()
    );
    let retired = fixture.logs();
    assert!(!fixture.dir(retired.trim_end()).exists());
    assert!(
        fixture
            .oauth
            .store
            .read(PROVIDER, None)
            .await
            .expect("store read")
            .is_none()
    );
}

#[tokio::test]
async fn absent_consent_refuses_oauth_login_without_creating_an_account_dir() {
    let fixture = Fixture::new(false);
    let result = fixture
        .oauth
        .login(&Fixture::interaction("", false, false))
        .await;
    assert!(
        result
            .expect_err("SD-4 refusal")
            .to_string()
            .contains("consent")
    );
    assert!(!Path::new(&fixture.oauth.cli.environment["CLAUDE_CONFIG_DIR"]).exists());
}

#[tokio::test]
async fn refresh_returns_the_same_cli_pool_without_touching_the_login() {
    let fixture = Fixture::new(true);
    fixture.login("", false).await;
    let credential = fixture
        .oauth
        .store
        .read(PROVIDER, None)
        .await
        .expect("read")
        .and_then(Credential::into_oauth)
        .expect("OAuth credential");
    let refreshed = fixture
        .oauth
        .refresh(&credential, &AbortController::new().signal())
        .await
        .expect("refresh");
    assert_eq!(refreshed, credential);
    assert_eq!(list_accounts(&refreshed, None).expect("list").len(), 1);
}
