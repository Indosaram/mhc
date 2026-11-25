//! The per-account Claude CLI login transaction behind `OAuthAuth::login`.

use crate::{
    accounts::{AccountSlot, assert_valid_account_name, empty_credential, stored_slots},
    claude_cli_account_dir::{
        RemoveAccountDirOptions, RemoveAccountDirOutcome, claude_cli_shared_root,
        ensure_account_dir, new_account_dir_id, remove_account_dir,
    },
    claude_cli_auth::{
        ClaudeCliLogin, ClaudeCliLoginEvent, ClaudeCliLoginFailure, ClaudeCliLoginMethod,
        ClaudeCliLoginOptions, ClaudeCliLoginResult, start_claude_cli_login,
    },
    claude_cli_auth_status::{ClaudeCliAuthStatus, read_claude_cli_auth_status},
    claude_cli_connection_test::{
        ClaudeCliCheckOutcome, ClaudeCliConnectionTestInput, connection_test_model,
        run_connection_test_with_profile,
    },
    claude_cli_login_outcome::{ClaudeCliLoginOutcome, record_claude_cli_login_outcome},
    claude_cli_login_slots::{signed_in_slot, with_check, with_slot, without_this_login},
    claude_cli_process::{ClaudeCliRun, DEFAULT_SHORT_COMMAND_TIMEOUT_MS},
    oauth_login::AnthropicSubscriptionOAuth,
    provider_id::ANTHROPIC_SUBSCRIPTION_PROVIDER_ID as PROVIDER,
};
use maho_ai::{
    auth::types::{
        AuthEvent, AuthInfoLink, AuthPrompt, AuthPromptKind, AuthPromptOption, Credential,
        OAuthCredential, ProviderAuthInteraction,
    },
    utils::abort::AbortController,
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

async fn account_name(
    current: &OAuthCredential,
    ui: &ProviderAuthInteraction,
) -> anyhow::Result<String> {
    let existing: Vec<_> = stored_slots(current)?
        .into_iter()
        .filter(|slot| slot.is_claude_cli() && !slot.name.trim().is_empty())
        .map(|slot| slot.name)
        .collect();
    if existing.is_empty() {
        return Ok("default".into());
    }
    let mut next = existing.len() + 1;
    while existing
        .iter()
        .any(|name| name == &format!("account-{next}"))
    {
        next += 1;
    }
    let fallback = format!("account-{next}");
    let answer = ui.prompt(AuthPrompt {
        kind: AuthPromptKind::Text {
            message: format!("Name for this account (existing: {}; press Enter to add {fallback}, or type an existing name to refresh it)", existing.join(", ")),
            placeholder: Some(fallback.clone()),
        },
        signal: Some(ui.signal.clone()),
    }).await?;
    Ok(if answer.trim().is_empty() {
        fallback
    } else {
        answer.trim().into()
    })
}

async fn sign_in(
    run: ClaudeCliRun<'_>,
    ui: &ProviderAuthInteraction,
    method: ClaudeCliLoginMethod,
) -> anyhow::Result<()> {
    let login_abort = AbortController::new();
    let ClaudeCliLogin {
        mut events,
        codes,
        mut result,
    } = start_claude_cli_login(ClaudeCliLoginOptions {
        run,
        method,
        signal: Some(login_abort.signal()),
        idle_timeout_ms: None,
        kill_grace_ms: None,
    });
    let paste_abort = AbortController::new();
    let paste_signal = paste_abort.signal();
    let mut paste = None;
    let mut open = true;
    let mut cancelled = false;
    let mut paste_error = None;
    let outcome = loop {
        tokio::select! {
            biased;
            () = ui.signal.cancelled(), if !cancelled => {
                cancelled = true;
                paste_abort.abort(None);
                paste = None;
                login_abort.abort(None);
            },
            event = events.recv(), if open && !cancelled => match event {
                Some(ClaudeCliLoginEvent::AutomaticUrl(url)) if method == ClaudeCliLoginMethod::Browser => {
                    ui.notify(AuthEvent::AuthUrl { url, instructions: None });
                }
                Some(ClaudeCliLoginEvent::ManualUrl(url)) => {
                    if method == ClaudeCliLoginMethod::CopyCode {
                        // AuthUrl automatically opens a browser in mhc's TUI. Copy-code only shows a link.
                        ui.notify(AuthEvent::Info {
                            message: "Open this URL on any device to get a code.".into(),
                            links: Some(vec![AuthInfoLink { url: url.clone(), label: None }]),
                        });
                    }
                    let codes = &codes;
                    let signal = paste_signal.clone();
                    paste = Some(Box::pin(async move {
                        loop {
                            let answer = ui.prompt(AuthPrompt {
                                kind: AuthPromptKind::ManualCode {
                                    message: "Paste the code Anthropic shows after you sign in:".into(),
                                    placeholder: Some(url.clone()),
                                },
                                signal: Some(signal.clone()),
                            }).await?;
                            if answer.trim().is_empty() || codes.submit_code(&answer) {
                                return Ok::<_, anyhow::Error>(());
                            }
                        }
                    }));
                }
                Some(ClaudeCliLoginEvent::AutomaticUrl(_)) => {}
                None => open = false,
            },
            outcome = &mut result => break outcome,
            pasted = async {
                if let Some(paste) = &mut paste { paste.await } else { std::future::pending().await }
            } => {
                paste = None;
                if let Err(error) = pasted {
                    paste_error = Some(error);
                    cancelled = true;
                    paste_abort.abort(None);
                    login_abort.abort(None);
                }
            }
        }
    };
    paste_abort.abort(None);
    if let Some(error) = paste_error {
        return Err(error);
    }
    match outcome {
        ClaudeCliLoginResult::Success => Ok(()),
        ClaudeCliLoginResult::Cancelled => anyhow::bail!("Login cancelled"),
        ClaudeCliLoginResult::Timeout => anyhow::bail!(
            "Claude sign-in timed out without a browser callback or a pasted code. Run the login again."
        ),
        ClaudeCliLoginResult::Failed(reason) => anyhow::bail!(
            "Claude sign-in failed: {}",
            match reason {
                ClaudeCliLoginFailure::OrgNotAllowed => "your organization does not allow it",
                ClaudeCliLoginFailure::ProviderNotAllowed =>
                    "your organization does not allow this provider",
                ClaudeCliLoginFailure::OAuthFailed => "OAuth login failed",
                ClaudeCliLoginFailure::UnexpectedExit => "the Claude Code CLI exited unexpectedly",
                ClaudeCliLoginFailure::SpawnFailed => "the Claude Code CLI could not start",
            }
        ),
    }
}

struct WrittenLogin {
    written: AccountSlot,
    previous: Option<AccountSlot>,
}

struct LoginTransaction {
    oauth: AnthropicSubscriptionOAuth,
    dir_id: String,
    executable: PathBuf,
    write: Arc<Mutex<Option<WrittenLogin>>>,
}

impl LoginTransaction {
    async fn rollback(self) -> anyhow::Result<RemoveAccountDirOutcome> {
        let write = self
            .write
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let store_result = if let Some(WrittenLogin { written, previous }) = write {
            self.oauth
                .store
                .modify(
                    PROVIDER,
                    Box::new(move |current| {
                        Box::pin(async move {
                            match current {
                                Some(Credential::OAuth(credential)) => Ok(Some(Credential::OAuth(
                                    without_this_login(credential, &written, previous.as_ref())?,
                                ))),
                                other => Ok(other),
                            }
                        })
                    }),
                    None,
                )
                .await
                .map(|_| ())
        } else {
            Ok(())
        };
        // If storage failed, do not delete a directory a still-written slot may own.
        store_result?;
        self.remove(&self.dir_id).await
    }

    async fn remove(&self, dir_id: &str) -> anyhow::Result<RemoveAccountDirOutcome> {
        remove_account_dir(RemoveAccountDirOptions {
            agent_dir: &self.oauth.cli.agent_dir,
            dir_id,
            executable: Some(&self.executable),
            parent_env: &self.oauth.cli.environment,
            signal: None,
        })
        .await
    }
}

// JS promises continue rolling back when Models.login loses its abort race; Rust drops a pending
// provider future. Keep the cleanup alive in that case as well.
struct PendingLogin(Option<LoginTransaction>);

impl Drop for PendingLogin {
    fn drop(&mut self) {
        if let Some(transaction) = self.0.take() {
            tokio::spawn(async move {
                match transaction.rollback().await {
                    Ok(RemoveAccountDirOutcome::Removed) => {}
                    Ok(
                        RemoveAccountDirOutcome::RemovedWithoutSignOut
                        | RemoveAccountDirOutcome::NotAnAccountDir,
                    )
                    | Err(_) => maho_ext_api::diagnostic(
                        "Claude sign-in was cancelled; cleanup could not confirm sign-out",
                    ),
                }
            });
        }
    }
}

pub(crate) async fn login(
    oauth: &AnthropicSubscriptionOAuth,
    ui: &ProviderAuthInteraction,
) -> anyhow::Result<OAuthCredential> {
    ui.signal.throw_if_aborted()?;
    // SAFETY-DIVERGENCE SD-4: account add and every caller of this OAuth flow must have recorded consent.
    crate::account_management::require_provider_consent(&oauth.cli.agent_dir)?;
    let current = match oauth.store.read(PROVIDER, None).await? {
        Some(Credential::OAuth(credential)) => credential,
        Some(Credential::ApiKey(_)) => {
            anyhow::bail!("Provider account management requires an OAuth credential")
        }
        None => empty_credential(),
    };
    let name = account_name(&current, ui).await?;
    assert_valid_account_name(&name)?;
    let method = ui
        .prompt(AuthPrompt {
            kind: AuthPromptKind::Select {
                message: "Select Anthropic login method:".into(),
                options: [
                    ("browser", "Browser login (default)"),
                    ("copy_code", "Copy code login (headless)"),
                ]
                .into_iter()
                .map(|(id, label)| AuthPromptOption {
                    id: id.into(),
                    label: label.into(),
                    description: None,
                })
                .collect(),
            },
            signal: Some(ui.signal.clone()),
        })
        .await?;
    let method = if method == "copy_code" || method == "Copy code login (headless)" {
        ClaudeCliLoginMethod::CopyCode
    } else {
        ClaudeCliLoginMethod::Browser
    };
    ui.signal.throw_if_aborted()?;
    let (executable, _) = (oauth.cli.resolve)()?;
    let dir_id = new_account_dir_id();
    let mut pending = PendingLogin(Some(LoginTransaction {
        oauth: oauth.clone(),
        dir_id: dir_id.clone(),
        executable: executable.clone(),
        write: Arc::new(Mutex::new(None)),
    }));
    let transaction = pending
        .0
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Missing Claude login transaction"))?;
    let result = conduct_login(transaction, ui, name, method).await;
    let transaction = pending
        .0
        .take()
        .ok_or_else(|| anyhow::anyhow!("Missing Claude login transaction"))?;
    match result {
        Ok(credential) => {
            let write = transaction
                .write
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(previous) = write.and_then(|write| write.previous)
                && previous.is_claude_cli()
                && let Some(dir_id) = previous.dir_id
            {
                match tokio::spawn(async move { transaction.remove(&dir_id).await }).await {
                    Ok(Ok(RemoveAccountDirOutcome::Removed)) => {}
                    Ok(Ok(RemoveAccountDirOutcome::RemovedWithoutSignOut)) => progress(
                        ui,
                        "Signed in; the previous Claude login could not be signed out, its dir was removed",
                    ),
                    Ok(Ok(RemoveAccountDirOutcome::NotAnAccountDir) | Err(_)) | Err(_) => progress(
                        ui,
                        "Signed in, but the previous Claude account dir could not be removed",
                    ),
                }
            }
            Ok(credential)
        }
        Err(error) => {
            match tokio::spawn(transaction.rollback()).await? {
                Ok(RemoveAccountDirOutcome::Removed) => {}
                Ok(
                    RemoveAccountDirOutcome::RemovedWithoutSignOut
                    | RemoveAccountDirOutcome::NotAnAccountDir,
                ) => progress(
                    ui,
                    "Claude sign-in was undone, but its Claude login could not be signed out",
                ),
                Err(cleanup) => {
                    return Err(cleanup.context(format!(
                        "Claude sign-in failed ({error}); cleanup failed too"
                    )));
                }
            }
            Err(error)
        }
    }
}

fn progress(ui: &ProviderAuthInteraction, message: impl Into<String>) {
    ui.notify(AuthEvent::Progress {
        message: message.into(),
    });
}

async fn conduct_login(
    transaction: &LoginTransaction,
    ui: &ProviderAuthInteraction,
    name: String,
    method: ClaudeCliLoginMethod,
) -> anyhow::Result<OAuthCredential> {
    let oauth = &transaction.oauth;
    let shared_root = claude_cli_shared_root(&oauth.cli.environment);
    let config_dir = ensure_account_dir(&oauth.cli.agent_dir, &transaction.dir_id, &shared_root)?;
    let run = || ClaudeCliRun {
        executable: &transaction.executable,
        config_dir: config_dir.clone(),
        parent_env: &oauth.cli.environment,
    };
    sign_in(run(), ui, method).await?;
    let status = read_claude_cli_auth_status(&run(), DEFAULT_SHORT_COMMAND_TIMEOUT_MS).await;
    ui.signal.throw_if_aborted()?;
    let profile = match status {
        ClaudeCliAuthStatus::SignedIn(profile) => profile,
        ClaudeCliAuthStatus::Rejected(_) => anyhow::bail!(
            "Claude sign-in did not produce a Claude subscription login (Pro/Max); sign in with your claude.ai account"
        ),
        ClaudeCliAuthStatus::SignedOut | ClaudeCliAuthStatus::Unreadable => anyhow::bail!(
            "Claude sign-in finished, but Claude Code reports no login for this account"
        ),
    };
    let write = transaction.write.clone();
    let dir_id = transaction.dir_id.clone();
    oauth
        .store
        .modify(
            PROVIDER,
            Box::new(move |current| {
                Box::pin(async move {
                    let base = match current {
                        Some(Credential::OAuth(credential)) => credential,
                        Some(Credential::ApiKey(_)) => anyhow::bail!(
                            "Provider account management requires an OAuth credential"
                        ),
                        None => empty_credential(),
                    };
                    let previous = stored_slots(&base)?
                        .into_iter()
                        .find(|candidate| candidate.name == name);
                    let written = signed_in_slot(previous.as_ref(), name, dir_id, profile);
                    let next = with_slot(base, &written)?;
                    *write
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        Some(WrittenLogin { written, previous });
                    Ok(Some(Credential::OAuth(next)))
                })
            }),
            None,
        )
        .await?;
    let slot = transaction
        .write
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .map(|write| write.written.clone())
        .ok_or_else(|| anyhow::anyhow!("Claude sign-in could not store its account"))?;

    ui.signal.throw_if_aborted()?;
    let settings = maho_core::settings_manager::SettingsManager::create(
        &oauth.cli.cwd.to_string_lossy(),
        &oauth.cli.agent_dir.to_string_lossy(),
        oauth
            .cli
            .environment
            .get("HOME")
            .map(String::as_str)
            .unwrap_or_default(),
        false,
    );
    let provider = settings.get_string("defaultProvider");
    let model = settings.get_string("defaultModel");
    let model = connection_test_model(provider.as_deref(), model.as_deref());
    progress(
        ui,
        format!("Testing the account with one turn on {model}..."),
    );
    let result = run_connection_test_with_profile(ClaudeCliConnectionTestInput {
        account: &slot,
        model,
        executable: &transaction.executable,
        cwd: &oauth.cli.cwd,
        environment: &oauth.cli.environment,
        agent_dir: &oauth.cli.agent_dir,
        signal: Some(&ui.signal),
        timeout_ms: None,
        now: &|| u64::try_from(maho_ai::utils::diagnostics::now_ms()).unwrap_or_default(),
    })
    .await;
    ui.signal.throw_if_aborted()?;
    match result.check.outcome {
        ClaudeCliCheckOutcome::Ok => progress(ui, format!("Connection test passed on {model}")),
        ClaudeCliCheckOutcome::Failed => progress(
            ui,
            format!(
                "Connection test failed on {model}: {}; the account was kept. Retry from the account list.",
                result
                    .check
                    .message
                    .as_deref()
                    .or(result.check.reason.as_deref())
                    .unwrap_or("No provider response was retained")
            ),
        ),
    }
    let account_name = slot.name.clone();
    let check = serde_json::to_value(result.check)?;
    let saved_check = check.clone();
    let profile = result.profile;
    let credential = oauth
        .store
        .modify(
            PROVIDER,
            Box::new(move |current| {
                Box::pin(async move {
                    match current {
                        Some(Credential::OAuth(credential)) => Ok(Some(Credential::OAuth(
                            with_check(credential, &slot, saved_check, profile)?,
                        ))),
                        other => Ok(other),
                    }
                })
            }),
            None,
        )
        .await?
        .and_then(Credential::into_oauth)
        .ok_or_else(|| {
            anyhow::anyhow!("Claude sign-in account was removed during its connection test")
        })?;
    record_claude_cli_login_outcome(
        &ui.signal,
        ClaudeCliLoginOutcome {
            account_name,
            check,
        },
    );
    Ok(credential)
}
