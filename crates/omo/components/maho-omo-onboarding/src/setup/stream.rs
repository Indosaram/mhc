//! The detection and planning half of `omo setup` as one stream (upstream
//! `omo-native/bin/lib/setup-stream.js`): each source is read, classified and reported on its own, in
//! the order later sources depend on. Nothing is written until a caller hands the collected plans to
//! [`NativePlans::apply`]. No source opens a socket or calls a model: the model-choice stage reads the
//! engine's offline model catalog only.
//!
//! [`NativeSetupBackend`] is the [`SetupBackend`] the engine's onboarding scan uses when the
//! environment names none; [`NativePlans`] is its [`SetupPlans`].

use super::SetupRuntime;
use super::agent_dir::canonical_agent_dir;
use super::assets_import::{AssetPlan, apply_assets, plan_assets};
use super::auth_store::KeyAddition;
use super::credentials::{
    CredentialPlan, WriteAuthFn, apply_credentials, opencode_oauth_providers, plan_credentials,
};
use super::detect::{Inventory, detect_harnesses};
use super::model_choices_import::{
    ModelChoicePlan, ModelChoiceStage, apply_model_choices, plan_model_choices,
};
use super::opencode_providers::{OpencodeProviders, PlannedProvider, plan_opencode_providers};
use super::providers_import::{ProviderPlan, apply_providers, plan_providers};
use super::stream_items;
use crate::scan_contract::{OnboardingApplyStage, OnboardingScanSource};
use crate::scan_runtime::{
    AuthAddition, OnboardingScanEnvironment, SetupApplyError, SetupBackend, SetupPlans,
    SetupStageResult, SetupStale, WriteAuth,
};
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

/// Every source, in stream order: a later source reads what an earlier one decided.
pub const SETUP_SOURCES: [OnboardingScanSource; 5] = [
    OnboardingScanSource::Detect,
    OnboardingScanSource::Providers,
    OnboardingScanSource::Credentials,
    OnboardingScanSource::Assets,
    OnboardingScanSource::ModelChoices,
];

/// `settle`: a source that fails yields its error and the stream goes on with the sources after it.
fn settle<T>(work: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).map_err(|panic| {
        panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_else(|| "the source failed".into())
    })
}

/// The stage plans `omo setup` renders and applies (`collectSetupPlans`' `plans`).
#[derive(Clone, Debug, Default)]
pub struct CollectedPlans {
    pub inventory: Inventory,
    pub credentials: Option<CredentialPlan>,
    pub assets: Option<AssetPlan>,
    pub providers: Option<ProviderPlan>,
    pub model_choices: Option<ModelChoicePlan>,
    /// Upstream `planChoices`: re-plans the model choices against other custom providers.
    pub replan: Option<Replan>,
}

/// What a model-choice re-plan reads.
#[derive(Clone)]
pub struct Replan {
    pub runtime: SetupRuntime,
    pub agent_dir: PathBuf,
    pub oauth_providers: HashSet<String>,
}

impl std::fmt::Debug for Replan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Replan")
            .field("agent_dir", &self.agent_dir)
            .finish_non_exhaustive()
    }
}

impl Replan {
    fn plan(&self, custom_providers: &[PlannedProvider]) -> ModelChoicePlan {
        plan_model_choices(&ModelChoiceStage {
            runtime: &self.runtime,
            agent_dir: &self.agent_dir,
            custom_providers,
            oauth_providers: &self.oauth_providers,
            load_registry: None,
        })
    }
}

/// `scanSetup` drained by `collectSetupPlans`: the stage rows a UI renders, and the plans of every
/// source that read without error. `on_source_start` is called right before an enabled source reads.
pub fn scan_setup(
    runtime: &SetupRuntime,
    agent_dir: &Path,
    include_custom_providers: bool,
    on_source_start: &mut dyn FnMut(OnboardingScanSource),
) -> (Vec<SetupStageResult>, CollectedPlans) {
    let mut results = Vec::new();
    let mut plans = CollectedPlans::default();
    let mut push = |source, items, error: Option<String>, disabled| {
        results.push(SetupStageResult {
            source,
            items,
            error,
            disabled,
        });
    };

    on_source_start(OnboardingScanSource::Detect);
    match settle(|| detect_harnesses(runtime)) {
        Ok(inventory) => {
            push(
                OnboardingScanSource::Detect,
                stream_items::detect_items(&inventory),
                None,
                false,
            );
            plans.inventory = inventory;
        }
        Err(error) => push(OnboardingScanSource::Detect, Vec::new(), Some(error), false),
    }

    if include_custom_providers {
        on_source_start(OnboardingScanSource::Providers);
    }
    let mut provider_source = OpencodeProviders::default();
    let providers = settle(|| {
        let source = if include_custom_providers {
            plan_opencode_providers(runtime)
        } else {
            OpencodeProviders::default()
        };
        let plan = plan_providers(&source, agent_dir);
        (source, plan)
    });
    match providers {
        Ok((source, plan)) => {
            let items = if include_custom_providers {
                stream_items::provider_items(&plan)
            } else {
                Vec::new()
            };
            push(
                OnboardingScanSource::Providers,
                items,
                None,
                !include_custom_providers,
            );
            provider_source = source;
            plans.providers = Some(plan);
        }
        Err(error) => push(
            OnboardingScanSource::Providers,
            Vec::new(),
            Some(error),
            !include_custom_providers,
        ),
    }

    on_source_start(OnboardingScanSource::Credentials);
    // A provider the custom-provider stage described is reported there only.
    let described: HashSet<String> = provider_source
        .providers
        .iter()
        .map(|p| p.id.clone())
        .chain(provider_source.skipped.iter().cloned())
        .collect();
    match settle(|| plan_credentials(runtime, &agent_dir.join("auth.json"), &described)) {
        Ok(plan) => {
            push(
                OnboardingScanSource::Credentials,
                stream_items::credential_items(&plan),
                None,
                false,
            );
            plans.credentials = Some(plan);
        }
        Err(error) => push(
            OnboardingScanSource::Credentials,
            Vec::new(),
            Some(error),
            false,
        ),
    }

    on_source_start(OnboardingScanSource::Assets);
    match settle(|| plan_assets(runtime, agent_dir)) {
        Ok(plan) => {
            push(
                OnboardingScanSource::Assets,
                stream_items::asset_items(&plan),
                None,
                false,
            );
            plans.assets = Some(plan);
        }
        Err(error) => push(OnboardingScanSource::Assets, Vec::new(), Some(error), false),
    }

    on_source_start(OnboardingScanSource::ModelChoices);
    let added: Vec<PlannedProvider> = plans
        .providers
        .as_ref()
        .map(|p| p.result.added.clone())
        .unwrap_or_default();
    let choices = settle(|| {
        let replan = Replan {
            runtime: runtime.clone(),
            agent_dir: agent_dir.to_path_buf(),
            oauth_providers: opencode_oauth_providers(runtime),
        };
        let plan = plan_model_choices(&ModelChoiceStage {
            runtime,
            agent_dir,
            custom_providers: &added,
            oauth_providers: &replan.oauth_providers,
            load_registry: None,
        });
        (plan, replan)
    });
    match choices {
        Ok((plan, replan)) => {
            push(
                OnboardingScanSource::ModelChoices,
                stream_items::model_choice_items(&plan),
                None,
                false,
            );
            plans.model_choices = Some(plan);
            plans.replan = Some(replan);
        }
        Err(error) => push(
            OnboardingScanSource::ModelChoices,
            Vec::new(),
            Some(error),
            false,
        ),
    }
    (results, plans)
}

fn resolve(path: &Path) -> String {
    std::path::absolute(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

// Every plan holds the bytes its target had when it was read. A target whose content no longer
// matches counts as stale; an absent file is its own expected state.
fn stale_target(path: &Path, expected: Option<&str>) -> bool {
    std::fs::read_to_string(path).ok().as_deref() != expected
}

impl CollectedPlans {
    /// `staleSetupTargets`: the target paths of `stages` whose content changed since their plans read
    /// them. Paths in `written` were rewritten by an earlier stage of the same apply.
    pub fn stale_targets(
        &self,
        stages: &[OnboardingApplyStage],
        written: &HashSet<String>,
    ) -> Vec<String> {
        let mut changed: Vec<String> = Vec::new();
        let mut add = |path: &Path| {
            let shown = path.to_string_lossy().into_owned();
            if !changed.contains(&shown) {
                changed.push(shown);
            }
        };
        let mut mark = |path: &Path, expected: Option<&str>, absent_only: bool| {
            if written.contains(&resolve(path)) {
                return;
            }
            // A skill directory planned as new is stale once anything exists at its path.
            let stale = if absent_only {
                path.exists()
            } else {
                stale_target(path, expected)
            };
            if stale {
                add(path);
            }
        };
        for stage in stages {
            match stage {
                OnboardingApplyStage::Credentials => {
                    let Some(plan) = &self.credentials else {
                        continue;
                    };
                    if plan.malformed || plan.pending == 0 {
                        continue;
                    }
                    mark(&plan.target, plan.current.bytes.as_deref(), false);
                }
                OnboardingApplyStage::Assets => {
                    let Some(plan) = self.assets.as_ref().filter(|p| p.present && p.pending > 0)
                    else {
                        continue;
                    };
                    if !plan.result.target.malformed {
                        mark(&plan.paths.mcp, plan.result.target.bytes.as_deref(), false);
                    }
                    for skill in &plan.result.skills.added {
                        mark(&plan.paths.skills.join(&skill.name), None, true);
                    }
                }
                OnboardingApplyStage::Providers => {
                    let Some(plan) = self
                        .providers
                        .as_ref()
                        .filter(|p| p.present && p.pending > 0)
                    else {
                        continue;
                    };
                    if !plan.models.malformed {
                        mark(&plan.paths.models, plan.models.bytes.as_deref(), false);
                    }
                    if !plan.result.keys.is_empty() {
                        mark(&plan.paths.auth, plan.auth_bytes.as_deref(), false);
                    }
                }
                OnboardingApplyStage::ModelChoices => {
                    let Some(plan) = self
                        .model_choices
                        .as_ref()
                        .filter(|p| p.present && p.pending > 0)
                    else {
                        continue;
                    };
                    for item in plan.items.iter().filter(|i| i.state == "pending") {
                        if let Some(target) = plan.target(item.target).filter(|t| !t.malformed) {
                            mark(&target.path, target.text.as_deref(), false);
                        }
                    }
                }
            }
        }
        changed
    }

    /// The files a stage writes when it applies (resolved).
    fn written_paths(&self, stage: OnboardingApplyStage) -> Vec<String> {
        match stage {
            OnboardingApplyStage::Credentials => self
                .credentials
                .iter()
                .map(|p| resolve(&p.target))
                .collect(),
            OnboardingApplyStage::Assets => self
                .assets
                .iter()
                .flat_map(|p| {
                    std::iter::once(resolve(&p.paths.mcp))
                        .chain(
                            p.result
                                .skills
                                .added
                                .iter()
                                .map(|s| resolve(&p.paths.skills.join(&s.name))),
                        )
                        .collect::<Vec<_>>()
                })
                .collect(),
            OnboardingApplyStage::Providers => self
                .providers
                .iter()
                .flat_map(|p| [resolve(&p.paths.models), resolve(&p.paths.auth)])
                .collect(),
            OnboardingApplyStage::ModelChoices => self
                .model_choices
                .iter()
                .flat_map(|p| {
                    p.items
                        .iter()
                        .filter_map(move |i| p.target(i.target).map(|t| resolve(&t.path)))
                })
                .collect(),
        }
    }

    /// `authWritesPending`: true when any of `stages` would write into a credential store.
    pub fn auth_writes_pending(&self, stages: &[OnboardingApplyStage]) -> bool {
        let credentials = stages.contains(&OnboardingApplyStage::Credentials)
            && self
                .credentials
                .as_ref()
                .is_some_and(|p| !p.malformed && p.pending > 0);
        let keys = stages.contains(&OnboardingApplyStage::Providers)
            && self
                .providers
                .as_ref()
                .is_some_and(|p| !p.result.keys.is_empty());
        credentials || keys
    }

    fn pending(&self, stage: OnboardingApplyStage) -> Option<usize> {
        match stage {
            OnboardingApplyStage::Credentials => self.credentials.as_ref().map(|p| p.pending),
            OnboardingApplyStage::Assets => self.assets.as_ref().map(|p| p.pending),
            OnboardingApplyStage::Providers => self.providers.as_ref().map(|p| p.pending),
            OnboardingApplyStage::ModelChoices => self.model_choices.as_ref().map(|p| p.pending),
        }
    }

    /// `applySetupStages`: applies every stage `accept` takes, in apply order (credentials, assets,
    /// providers, model choices), and returns the stages it applied. A declined provider stage leaves
    /// models.json as it is, so the model choices are re-planned without its providers. With
    /// `stale_guard`, a stage whose target changed since planning refuses the apply.
    ///
    /// # Errors
    /// A stale target, or a writer failed.
    pub fn apply_stages(
        &mut self,
        accept: &dyn Fn(OnboardingApplyStage) -> bool,
        report: &mut dyn FnMut(&str),
        mut write_auth: Option<WriteAuthFn<'_>>,
        stale_guard: bool,
    ) -> Result<Vec<OnboardingApplyStage>, SetupApplyError> {
        let mut applied: Vec<OnboardingApplyStage> = Vec::new();
        let mut written: HashSet<String> = HashSet::new();
        let order = [
            OnboardingApplyStage::Credentials,
            OnboardingApplyStage::Assets,
            OnboardingApplyStage::Providers,
            OnboardingApplyStage::ModelChoices,
        ];
        for stage in order {
            if stage == OnboardingApplyStage::ModelChoices
                && self.model_choices.is_some()
                && self.providers.as_ref().is_some_and(|p| p.pending > 0)
                && !applied.contains(&OnboardingApplyStage::Providers)
                && let Some(replan) = &self.replan
            {
                let shown: HashSet<String> = self
                    .model_choices
                    .iter()
                    .flat_map(|p| p.notices.iter().cloned())
                    .collect();
                let next = replan.plan(&[]);
                for notice in next.notices.iter().filter(|n| !shown.contains(*n)) {
                    report(notice);
                }
                self.model_choices = Some(next);
            }
            if self.pending(stage).is_none_or(|p| p == 0) || !accept(stage) {
                continue;
            }
            if stale_guard {
                let stale = self.stale_targets(&[stage], &written);
                if !stale.is_empty() {
                    return Err(SetupApplyError::Stale(SetupStale {
                        paths: stale,
                        applied,
                    }));
                }
            }
            let failed = |error: String| SetupApplyError::Failed(error);
            match stage {
                OnboardingApplyStage::Credentials => {
                    if let Some(plan) = &self.credentials {
                        apply_credentials(
                            plan,
                            write_auth.as_deref_mut().map(|w| w as WriteAuthFn<'_>),
                        )
                        .map_err(failed)?;
                    }
                }
                OnboardingApplyStage::Assets => {
                    if let Some(plan) = &self.assets {
                        apply_assets(plan).map_err(failed)?;
                    }
                }
                OnboardingApplyStage::Providers => {
                    if let Some(plan) = &mut self.providers {
                        plan.result.keys = apply_providers(
                            plan,
                            write_auth.as_deref_mut().map(|w| w as WriteAuthFn<'_>),
                        )
                        .map_err(failed)?;
                    }
                }
                OnboardingApplyStage::ModelChoices => {
                    if let Some(plan) = &mut self.model_choices {
                        apply_model_choices(plan, report);
                    }
                }
            }
            applied.push(stage);
            written.extend(self.written_paths(stage));
        }
        Ok(applied)
    }
}

/// The [`SetupPlans`] of one native scan.
pub struct NativePlans {
    plans: Mutex<CollectedPlans>,
}

impl NativePlans {
    pub fn new(plans: CollectedPlans) -> Self {
        Self {
            plans: Mutex::new(plans),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CollectedPlans> {
        self.plans.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A snapshot of the plans.
    pub fn snapshot(&self) -> CollectedPlans {
        self.lock().clone()
    }
}

impl SetupPlans for NativePlans {
    fn stale_targets(
        &self,
        stages: &[OnboardingApplyStage],
        written: &HashSet<String>,
    ) -> Vec<String> {
        self.lock().stale_targets(stages, written)
    }

    fn auth_writes_pending(&self, stages: &[OnboardingApplyStage]) -> bool {
        self.lock().auth_writes_pending(stages)
    }

    fn auth_target(&self) -> Option<String> {
        let plans = self.lock();
        plans
            .credentials
            .as_ref()
            .map(|p| p.target.to_string_lossy().into_owned())
            .or_else(|| {
                plans
                    .providers
                    .as_ref()
                    .map(|p| p.paths.auth.to_string_lossy().into_owned())
            })
    }

    fn apply(
        &self,
        stages: &[OnboardingApplyStage],
        write_auth: Option<WriteAuth<'_>>,
        report: &mut dyn FnMut(&str),
    ) -> Result<Vec<OnboardingApplyStage>, SetupApplyError> {
        let mut plans = self.lock();
        let accept = |stage: OnboardingApplyStage| stages.contains(&stage);
        match write_auth {
            Some(store) => {
                let mut adapt = |target: &str, additions: &[KeyAddition]| {
                    let converted: Vec<AuthAddition> = additions
                        .iter()
                        .map(|a| AuthAddition {
                            provider: a.provider.clone(),
                            key: a.key.clone(),
                        })
                        .collect();
                    store(target, &converted)
                };
                plans.apply_stages(&accept, report, Some(&mut adapt), true)
            }
            None => plans.apply_stages(&accept, report, None, true),
        }
    }
}

/// `omo setup`'s own detection on this machine: the [`SetupBackend`] the onboarding scan uses when the
/// environment names none. One instance serves one scan: [`SetupBackend::plans`] hands out the plans
/// its [`SetupBackend::scan`] collected.
#[derive(Default)]
pub struct NativeSetupBackend {
    /// Replaces the engine's builtin model catalog (tests).
    pub load_model_catalog: Option<super::engine_models::LoadCatalog>,
    /// The plugin root whose `skills/` holds the bundled skills.
    pub plugin_root: Option<PathBuf>,
    collected: Mutex<VecDeque<CollectedPlans>>,
}

impl NativeSetupBackend {
    pub fn runtime(&self, environment: &OnboardingScanEnvironment) -> (SetupRuntime, PathBuf) {
        let home = environment.home_dir();
        let env = environment.env_map();
        let agent_dir = environment
            .agent_dir
            .as_ref()
            .map_or_else(|| canonical_agent_dir(&env, &home), PathBuf::from);
        let mut runtime = SetupRuntime::new(home, env);
        runtime.load_model_catalog = self.load_model_catalog.clone();
        runtime.plugin_root = self.plugin_root.clone();
        (runtime, agent_dir)
    }
}

impl SetupBackend for NativeSetupBackend {
    fn scan(
        &self,
        environment: &OnboardingScanEnvironment,
        include_custom_providers: bool,
        on_source_start: &mut dyn FnMut(OnboardingScanSource),
    ) -> Vec<SetupStageResult> {
        let (runtime, agent_dir) = self.runtime(environment);
        let (results, plans) = scan_setup(
            &runtime,
            &agent_dir,
            include_custom_providers,
            on_source_start,
        );
        self.collected
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(plans);
        results
    }

    fn plans(&self, results: &[SetupStageResult]) -> Arc<dyn SetupPlans> {
        let mut plans = self
            .collected
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front()
            .unwrap_or_default();
        // A failed source has no plan, so its stage is never applied.
        let ok = |source: OnboardingScanSource| {
            results
                .iter()
                .any(|r| r.source == source && r.error.is_none())
        };
        if !ok(OnboardingScanSource::Credentials) {
            plans.credentials = None;
        }
        if !ok(OnboardingScanSource::Assets) {
            plans.assets = None;
        }
        if !ok(OnboardingScanSource::Providers) {
            plans.providers = None;
        }
        if !ok(OnboardingScanSource::ModelChoices) {
            plans.model_choices = None;
        }
        Arc::new(NativePlans::new(plans))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::engine_models::CatalogProvider;
    use serde_json::{Value, json};
    use std::collections::BTreeMap;

    const SECRET_A: &str = "SK-SENTINEL-STREAM-A";
    const SECRET_B: &str = "SK-SENTINEL-STREAM-B";

    struct Fixture {
        _dir: tempfile::TempDir,
        runtime: SetupRuntime,
        agent_dir: PathBuf,
        catalog_calls: Arc<Mutex<Vec<&'static str>>>,
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        write(
            &home.join(".local/share/opencode/auth.json"),
            &json!({
                "openai": {"type": "api", "key": SECRET_A},
                "anthropic": {"type": "oauth", "refresh": "r", "access": "a", "expires": 1},
                "myco": {"type": "api", "key": SECRET_B},
            })
            .to_string(),
        );
        write(
            &home.join(".config/opencode/opencode.json"),
            &json!({
                "model": "openai/gpt-5",
                "mcp": {"ctx": {"type": "local", "command": ["npx", "ctx"]}},
                "provider": {"myco": {"npm": "@ai-sdk/openai-compatible", "options": {"baseURL": "https://example.invalid/v1"}, "models": {"m1": {"name": "M1"}}}},
            })
            .to_string(),
        );
        write(
            &home.join(".config/opencode/skills/my-skill/SKILL.md"),
            "---\nname: my-skill\ndescription: d\n---\nbody\n",
        );
        let mut env = BTreeMap::new();
        env.insert("HOME".into(), home.to_string_lossy().into_owned());
        let mut runtime = SetupRuntime::new(home.to_string_lossy(), env);
        let calls: Arc<Mutex<Vec<&'static str>>> = Arc::default();
        let log = Arc::clone(&calls);
        runtime.load_model_catalog =
            Some(Arc::new(move || -> Result<Vec<CatalogProvider>, String> {
                log.lock().expect("log").push("catalog");
                Ok(vec![CatalogProvider {
                    id: "openai".into(),
                    models: vec!["gpt-5".into()],
                    refreshes: false,
                }])
            }));
        runtime.plugin_root = Some(dir.path().join("no-plugin"));
        Fixture {
            agent_dir: super::super::agent_dir::default_agent_dir(&home),
            runtime,
            catalog_calls: calls,
            _dir: dir,
        }
    }

    fn has(
        items: &[crate::scan_contract::OnboardingScanItem],
        id: &str,
        kind: &str,
        state: &str,
    ) -> bool {
        items
            .iter()
            .any(|i| i.id == id && i.kind == kind && i.state == state)
    }

    fn read(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json")
    }

    #[test]
    fn every_stage_arrives_once_in_order_with_its_items_and_no_key() {
        let f = fixture();
        let (results, _) = scan_setup(&f.runtime, &f.agent_dir, true, &mut |_| {});
        assert_eq!(
            results.iter().map(|r| r.source).collect::<Vec<_>>(),
            SETUP_SOURCES.to_vec()
        );
        let items = |source: OnboardingScanSource| {
            &results
                .iter()
                .find(|r| r.source == source)
                .expect("stage")
                .items
        };
        assert!(has(
            items(OnboardingScanSource::Detect),
            "opencode",
            "harness",
            "found"
        ));
        assert!(has(
            items(OnboardingScanSource::Providers),
            "myco",
            "provider",
            "import"
        ));
        assert!(has(
            items(OnboardingScanSource::Credentials),
            "openai",
            "api-key",
            "import"
        ));
        assert!(has(
            items(OnboardingScanSource::Credentials),
            "anthropic",
            "oauth",
            "login"
        ));
        assert!(has(
            items(OnboardingScanSource::Assets),
            "ctx",
            "mcp",
            "import"
        ));
        assert!(has(
            items(OnboardingScanSource::Assets),
            "my-skill",
            "skill",
            "import"
        ));
        assert!(has(
            items(OnboardingScanSource::ModelChoices),
            "default model",
            "default",
            "import"
        ));
        let serialized =
            serde_json::to_string(&results.iter().map(|r| &r.items).collect::<Vec<_>>())
                .expect("json");
        assert!(!serialized.contains(SECRET_A) && !serialized.contains(SECRET_B));
    }

    #[test]
    fn custom_providers_off_disables_the_provider_stage_and_leaves_its_key_unmapped() {
        let f = fixture();
        let mut started = Vec::new();
        let (results, plans) =
            scan_setup(&f.runtime, &f.agent_dir, false, &mut |s| started.push(s));
        let providers = results
            .iter()
            .find(|r| r.source == OnboardingScanSource::Providers)
            .expect("providers");
        assert!(providers.disabled && providers.items.is_empty());
        assert_eq!(plans.providers.as_ref().map(|p| p.pending), Some(0));
        assert!(!started.contains(&OnboardingScanSource::Providers));
        let credentials = results
            .iter()
            .find(|r| r.source == OnboardingScanSource::Credentials)
            .expect("credentials");
        assert!(has(&credentials.items, "myco", "api-key", "unmapped"));
    }

    /// Upstream reads one stage at a time and the model catalog loads only with the last one.
    #[test]
    fn the_model_catalog_loads_only_once_the_model_choice_stage_starts() {
        let f = fixture();
        let log = Arc::clone(&f.catalog_calls);
        let mut on_start = |source: OnboardingScanSource| {
            if source == OnboardingScanSource::ModelChoices {
                log.lock().expect("log").push("model-choices started");
            }
        };
        scan_setup(&f.runtime, &f.agent_dir, true, &mut on_start);
        assert_eq!(
            *f.catalog_calls.lock().expect("log"),
            vec!["model-choices started", "catalog"]
        );
    }

    #[test]
    fn accepting_only_credentials_writes_only_auth_json() {
        let f = fixture();
        let (_, mut plans) = scan_setup(&f.runtime, &f.agent_dir, true, &mut |_| {});
        assert!(plans.inventory.harnesses.iter().any(|h| h.id == "opencode"));
        let mut reported = Vec::new();
        let applied = plans
            .apply_stages(
                &|s| s == OnboardingApplyStage::Credentials,
                &mut |l| reported.push(l.to_owned()),
                None,
                false,
            )
            .expect("apply");
        assert_eq!(applied, vec![OnboardingApplyStage::Credentials]);
        assert!(read(&f.agent_dir.join("auth.json")).get("openai").is_some());
        for absent in ["mcp.json", "models.json", "settings.json"] {
            assert!(!f.agent_dir.join(absent).exists(), "{absent}");
        }
    }

    #[test]
    fn a_target_edited_after_planning_refuses_the_apply_and_names_the_path() {
        let f = fixture();
        let (_, mut plans) = scan_setup(&f.runtime, &f.agent_dir, true, &mut |_| {});
        let auth = f.agent_dir.join("auth.json");
        write(
            &auth,
            &json!({"anthropic": {"type": "oauth", "access": "a2", "refresh": "r2", "expires": 2}})
                .to_string(),
        );
        let error = plans
            .apply_stages(&|_| true, &mut |_| {}, None, true)
            .expect_err("stale");
        let SetupApplyError::Stale(stale) = error else {
            panic!("expected stale")
        };
        assert_eq!(stale.paths, vec![auth.to_string_lossy().into_owned()]);
        assert!(stale.applied.is_empty());
        let stored = read(&auth);
        assert_eq!(stored["anthropic"]["refresh"], json!("r2"));
        assert!(stored.get("openai").is_none());
    }

    #[test]
    fn a_skill_directory_created_after_planning_counts_as_changed() {
        let f = fixture();
        let (_, plans) = scan_setup(&f.runtime, &f.agent_dir, true, &mut |_| {});
        let skill_dir = f.agent_dir.join("skills").join("my-skill");
        let shown = skill_dir.to_string_lossy().into_owned();
        assert!(
            !plans
                .stale_targets(&[OnboardingApplyStage::Assets], &HashSet::new())
                .contains(&shown)
        );
        std::fs::create_dir_all(&skill_dir).expect("mkdir");
        assert!(
            plans
                .stale_targets(&[OnboardingApplyStage::Assets], &HashSet::new())
                .contains(&shown)
        );
    }

    #[test]
    fn keys_land_through_the_store_and_merge_with_what_it_holds() {
        let f = fixture();
        let backend = NativeSetupBackend {
            load_model_catalog: f.runtime.load_model_catalog.clone(),
            plugin_root: f.runtime.plugin_root.clone(),
            ..Default::default()
        };
        let environment = OnboardingScanEnvironment {
            home: Some(f.runtime.home.clone()),
            env: Some(f.runtime.env.clone()),
            ..Default::default()
        };
        let results = backend.scan(&environment, false, &mut |_| {});
        let plans = backend.plans(&results);
        let target = f.agent_dir.join("auth.json");
        assert_eq!(
            plans.auth_target(),
            Some(target.to_string_lossy().into_owned())
        );
        assert!(plans.auth_writes_pending(&[OnboardingApplyStage::Credentials]));
        let mut calls: Vec<String> = Vec::new();
        let mut store = |path: &str, additions: &[AuthAddition]| -> Result<(), String> {
            assert_eq!(path, target.to_string_lossy());
            let mut entries = std::fs::read_to_string(path)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .unwrap_or_else(|| json!({}));
            for item in additions {
                calls.push(item.provider.clone());
                entries[&item.provider] = json!({"type": "api_key", "key": item.key});
            }
            write(Path::new(path), &entries.to_string());
            Ok(())
        };
        let applied = plans
            .apply(
                &[OnboardingApplyStage::Credentials],
                Some(&mut store),
                &mut |_| {},
            )
            .expect("apply");
        assert_eq!(applied, vec![OnboardingApplyStage::Credentials]);
        assert_eq!(calls, vec!["openai"]);
        let auth = read(&target);
        assert_eq!(auth["openai"], json!({"type": "api_key", "key": SECRET_A}));
        assert!(auth.get("anthropic").is_none());
    }
}
