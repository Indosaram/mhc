//! `manager/parent-registry-context.test.ts`, against an in-crate fake of the host registry.

use std::sync::{Arc, Mutex};

use crate::manager::parent_registry_context::{
    ChildModelRegistry, ParentModelRegistryResolver, create_parent_registry_session_context,
    find_model_reference, resolve_resume_context,
};
use crate::manager::runner::{InProcessSessionContext, InProcessSessionContextProvider};
use crate::manager::types::ManagedStartSpec;
use crate::runners::in_process::child_options::HostHandle;
use crate::state::{ResolvedModelRecord, ResolvedModelSource};

#[derive(Debug, PartialEq, Eq)]
struct FakeModel {
    provider: String,
    id: String,
}

struct FakeRegistry {
    models: Vec<(&'static str, &'static str)>,
    auth_storage: HostHandle,
    model_runtime: HostHandle,
}

impl ChildModelRegistry for FakeRegistry {
    fn find(&self, provider: &str, model_id: &str) -> Option<HostHandle> {
        self.models
            .iter()
            .any(|(known_provider, known_id)| *known_provider == provider && *known_id == model_id)
            .then(|| {
                Arc::new(FakeModel {
                    provider: provider.to_string(),
                    id: model_id.to_string(),
                }) as HostHandle
            })
    }

    fn auth_storage(&self) -> HostHandle {
        Arc::clone(&self.auth_storage)
    }

    fn model_runtime(&self) -> Option<HostHandle> {
        Some(Arc::clone(&self.model_runtime))
    }
}

fn registry_with(models: Vec<(&'static str, &'static str)>) -> Arc<FakeRegistry> {
    Arc::new(FakeRegistry {
        models,
        auth_storage: Arc::new("auth-storage"),
        model_runtime: Arc::new("model-runtime"),
    })
}

fn registry_with_mock_provider() -> Arc<FakeRegistry> {
    registry_with(vec![("omo-mock", "mock-1")])
}

fn resolver(registry: Option<Arc<FakeRegistry>>) -> ParentModelRegistryResolver {
    Arc::new(move || {
        registry
            .clone()
            .map(|registry| registry as Arc<dyn ChildModelRegistry>)
    })
}

fn base_spec() -> ManagedStartSpec {
    ManagedStartSpec {
        task_id: "st_child".to_string(),
        cwd: "/tmp/project".to_string(),
        state_dir: "/tmp/project/.omo/task".to_string(),
        prompt: "do the child work".to_string(),
        depth: 1,
        parent_session_id: "parent-session".to_string(),
        root_session_id: "parent-session".to_string(),
        ..ManagedStartSpec::default()
    }
}

fn with_model(model: &str) -> ManagedStartSpec {
    ManagedStartSpec {
        model: Some(model.to_string()),
        ..base_spec()
    }
}

fn with_resolved(model_id: &str, display: &str) -> ManagedStartSpec {
    ManagedStartSpec {
        resolved_model: Some(ResolvedModelRecord {
            display: display.to_string(),
            ..ResolvedModelRecord::new(ResolvedModelSource::Explicit, "omo-mock", model_id)
        }),
        ..base_spec()
    }
}

fn model_of(context: &InProcessSessionContext) -> Option<&FakeModel> {
    context.model.as_ref()?.downcast_ref::<FakeModel>()
}

fn same(handle: Option<&HostHandle>, expected: &HostHandle) -> bool {
    handle.is_some_and(|handle| Arc::ptr_eq(handle, expected))
}

fn is_registry(context: &InProcessSessionContext, registry: &Arc<FakeRegistry>) -> bool {
    let expected: HostHandle = Arc::clone(registry) as HostHandle;
    same(context.model_registry.as_ref(), &expected)
}

fn mock_model(id: &str) -> FakeModel {
    FakeModel {
        provider: "omo-mock".to_string(),
        id: id.to_string(),
    }
}

// findModelReference

#[test]
fn given_canonical_reference_when_resolved_then_find_called_with_split_parts() {
    let calls = Mutex::new(Vec::new());

    let model = find_model_reference(
        |provider, id| {
            calls
                .lock()
                .expect("calls")
                .push((provider.to_string(), id.to_string()));
            Some((provider.to_string(), id.to_string()))
        },
        "omo-mock/mock-1",
    );

    assert_eq!(
        calls.into_inner().expect("calls"),
        vec![("omo-mock".to_string(), "mock-1".to_string())]
    );
    assert_eq!(model, Some(("omo-mock".to_string(), "mock-1".to_string())));
}

#[test]
fn given_model_id_with_slashes_when_resolved_then_only_first_slash_splits() {
    let calls = Mutex::new(Vec::new());

    let model: Option<()> = find_model_reference(
        |provider, id| {
            calls
                .lock()
                .expect("calls")
                .push((provider.to_string(), id.to_string()));
            None
        },
        "openrouter/anthropic/claude-3.5",
    );

    assert_eq!(model, None);
    assert_eq!(
        calls.into_inner().expect("calls"),
        vec![("openrouter".to_string(), "anthropic/claude-3.5".to_string())]
    );
}

#[test]
fn given_reference_without_usable_slash_when_resolved_then_none_without_calling_find() {
    let called = Mutex::new(false);
    let find = |_: &str, _: &str| {
        *called.lock().expect("called") = true;
        Some(())
    };

    assert_eq!(find_model_reference(find, "no-slash"), None);
    assert_eq!(find_model_reference(find, "/leading"), None);
    assert_eq!(find_model_reference(find, "trailing/"), None);
    assert!(!*called.lock().expect("called"));
}

// createParentRegistrySessionContext

#[test]
fn given_no_parent_registry_when_context_built_then_empty() {
    let provider = create_parent_registry_session_context(resolver(None));

    let context = provider.provide(&with_model("omo-mock/mock-1"));

    assert!(context.model_registry.is_none());
    assert!(context.auth_storage.is_none());
    assert!(context.model_runtime.is_none());
    assert!(context.model.is_none());
    assert_eq!(context.thinking_level, None);
    assert_eq!(context.agent_dir, None);
}

#[test]
fn given_registry_with_dynamic_provider_when_spec_names_model_then_registry_auth_and_model_threaded()
 {
    let registry = registry_with_mock_provider();
    let provider = create_parent_registry_session_context(resolver(Some(Arc::clone(&registry))));

    let context = provider.provide(&with_model("omo-mock/mock-1"));

    assert!(is_registry(&context, &registry));
    assert!(same(
        context.model_runtime.as_ref(),
        &registry.model_runtime
    ));
    assert!(same(context.auth_storage.as_ref(), &registry.auth_storage));
    assert_eq!(model_of(&context), Some(&mock_model("mock-1")));
}

#[test]
fn given_registry_but_no_model_on_spec_when_context_built_then_no_model_override() {
    let registry = registry_with_mock_provider();
    let provider = create_parent_registry_session_context(resolver(Some(Arc::clone(&registry))));

    let context = provider.provide(&base_spec());

    assert!(is_registry(&context, &registry));
    assert!(same(context.auth_storage.as_ref(), &registry.auth_storage));
    assert!(context.model.is_none());
}

#[test]
fn given_valid_resolved_variant_when_context_built_then_maps_to_thinking_level() {
    let provider =
        create_parent_registry_session_context(resolver(Some(registry_with_mock_provider())));

    let context = provider.provide(&ManagedStartSpec {
        variant: Some("xhigh".to_string()),
        ..with_model("omo-mock/mock-1")
    });

    assert_eq!(context.thinking_level.as_deref(), Some("xhigh"));
}

#[test]
fn given_unknown_variant_when_context_built_then_no_thinking_level() {
    let provider =
        create_parent_registry_session_context(resolver(Some(registry_with_mock_provider())));

    let context = provider.provide(&ManagedStartSpec {
        variant: Some("ultra".to_string()),
        ..with_model("omo-mock/mock-1")
    });

    assert_eq!(context.thinking_level, None);
}

#[test]
fn given_model_absent_from_registry_when_context_built_then_registry_threaded_without_model() {
    let registry = registry_with_mock_provider();
    let provider = create_parent_registry_session_context(resolver(Some(Arc::clone(&registry))));

    let context = provider.provide(&with_model("omo-mock/does-not-exist"));

    assert!(is_registry(&context, &registry));
    assert!(context.model.is_none());
}

// resolveResumeContext

#[test]
fn given_resolved_model_in_live_registry_when_resumed_then_ok_with_exact_model_and_registry() {
    let registry = registry_with_mock_provider();

    let context = resolve_resume_context(
        &resolver(Some(Arc::clone(&registry))),
        &with_resolved("mock-1", "Mock 1"),
    )
    .expect("resolved");

    assert!(is_registry(&context, &registry));
    assert!(same(context.auth_storage.as_ref(), &registry.auth_storage));
    assert_eq!(model_of(&context), Some(&mock_model("mock-1")));
}

#[test]
fn given_resolved_model_removed_from_registry_when_resumed_then_model_unavailable_naming_it() {
    let reason = resolve_resume_context(
        &resolver(Some(registry_with_mock_provider())),
        &with_resolved("does-not-exist", "Mock 1"),
    )
    .err()
    .expect("model unavailable");

    assert!(reason.contains("omo-mock"), "{reason}");
    assert!(reason.contains("does-not-exist"), "{reason}");
}

#[test]
fn given_display_differing_from_canonical_id_when_resumed_then_keys_on_provider_and_model_id() {
    let context = resolve_resume_context(
        &resolver(Some(registry_with_mock_provider())),
        &with_resolved("mock-1", "Some Human Label That Does Not Match"),
    )
    .expect("resolved");

    assert_eq!(model_of(&context), Some(&mock_model("mock-1")));
}

#[test]
fn given_display_matching_a_different_model_when_resumed_then_provider_model_id_match_wins() {
    let registry = registry_with(vec![("omo-mock", "mock-1"), ("omo-mock", "mock-2")]);

    let context = resolve_resume_context(
        &resolver(Some(registry)),
        &with_resolved("mock-1", "Mock 2"),
    )
    .expect("resolved");

    assert_eq!(model_of(&context), Some(&mock_model("mock-1")));
}

#[test]
fn given_no_live_registry_when_resumed_then_model_unavailable() {
    let result = resolve_resume_context(&resolver(None), &with_resolved("mock-1", "Mock 1"));

    assert!(result.is_err());
}

#[test]
fn given_spec_without_resolved_model_when_resumed_then_model_unavailable() {
    let result =
        resolve_resume_context(&resolver(Some(registry_with_mock_provider())), &base_spec());

    assert!(result.is_err());
}
