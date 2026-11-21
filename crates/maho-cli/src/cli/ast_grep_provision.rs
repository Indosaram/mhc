//! Session-start ast-grep provisioning (omo profile).
//!
//! Port of the upstream senpi plugin hook (`hook.ts`): on `session-created` it starts one
//! background provisioning attempt per process and stays out of the way otherwise. The hook
//! reacts to session start only, returns immediately when a binary already resolves, claims the
//! attempt before spawning so concurrent sessions do not race, and never fails the session:
//! provisioning errors are logged to stderr and dropped.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use maho_ext_api::{EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent};
use omo_utils::ast_grep::{
    SgCacheMode, SgFetchResponse, SgProvisionOptions, SgResolution, SgResolverOptions,
    ast_grep_runtime_dir, provision_sg_binary, resolve_sg_binary_sync,
};
use omo_utils::runtime::{node_arch, node_platform};

/// One provisioning attempt per runtime directory per process, so repeated session starts
/// (subagents, resumed sessions) do not download the same release again.
fn claimed() -> &'static Mutex<HashSet<String>> {
    static CLAIMED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    CLAIMED.get_or_init(|| Mutex::new(HashSet::new()))
}

/// `<home>/.omo/runtime/ast-grep/<platform>-<arch>`, the `OmoRuntime` candidate the resolver
/// probes before `PATH`. The `.omo` segment is literal because the resolver hardcodes it
/// (`candidates.rs`); using the brand `configDir` here would provision into a directory the
/// resolver never probes.
fn runtime_dir() -> String {
    let base = format!("{}/.omo", maho_core::config::home_dir());
    ast_grep_runtime_dir(&base, node_platform(), node_arch())
}

/// Downloads the pinned release with a blocking HTTP client on the provisioning thread.
fn fetch(url: &str) -> Result<SgFetchResponse, String> {
    let client = reqwest::blocking::Client::builder()
        .build()
        .map_err(|error| error.to_string())?;
    let response = client.get(url).send().map_err(|error| error.to_string())?;
    let status = response.status().as_u16();
    let body = response
        .bytes()
        .map_err(|error| error.to_string())?
        .to_vec();
    Ok(SgFetchResponse { status, body })
}

/// The provisioning target, exposed so tests can assert it matches the resolver candidate.
pub fn runtime_dir_for_test() -> String {
    runtime_dir()
}

fn provision_in_background(target_dir: PathBuf) {
    let spawned = std::thread::Builder::new()
        .name("ast-grep-provision".into())
        .spawn(move || {
            // Cheap exit: a resolvable binary means there is nothing to do. Runs on this thread
            // so the session-start path never waits on a filesystem probe or a version spawn.
            let probe = SgResolverOptions {
                cache: SgCacheMode::Bypass,
                runtime_dir: Some(&target_dir.to_string_lossy()),
                ..SgResolverOptions::default()
            };
            if matches!(resolve_sg_binary_sync(&probe), SgResolution::Found { .. }) {
                return;
            }
            let options = SgProvisionOptions {
                arch: None,
                fetch: &fetch,
                platform: None,
                release_assets: None,
                target_dir,
            };
            // Upstream logs failures only; a successful download stays silent.
            if let Err(error) = provision_sg_binary(&options) {
                maho_ext_api::diagnostic(&format!(
                    "ast-grep: background provisioning failed ({error})"
                ));
            }
        });
    if let Err(error) = spawned {
        maho_ext_api::diagnostic(&format!("ast-grep: could not start provisioning ({error})"));
    }
}

/// Registers the session-start provisioning hook.
#[derive(Default)]
pub struct AstGrepProvisionExtension;

impl Extension for AstGrepProvisionExtension {
    fn register(&self, api: &mut ExtensionApi) {
        api.on(
            EventKind::SessionStart,
            std::sync::Arc::new(move |_event: &mut ExtensionEvent, _ctx| {
                Box::pin(async move {
                    let dir = runtime_dir();
                    let first = claimed()
                        .lock()
                        .map(|mut set| set.insert(dir.clone()))
                        .unwrap_or(false);
                    if first {
                        provision_in_background(PathBuf::from(dir));
                    }
                    Ok(EventResult::None)
                })
            }),
        );
    }
}
