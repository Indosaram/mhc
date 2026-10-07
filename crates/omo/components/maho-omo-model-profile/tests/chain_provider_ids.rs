pub mod support;

use maho_omo_model_profile::builtin_profiles::builtin_model_profiles;

// Port of the pinned engine's `builtinProviders()` registry check. The native side has no engine
// registry accessor at this layer yet (recorded as a seam in `.omo/authoring/model-profile.md`), so
// the engine id set is carried here as a const; the check still fails on a typo'd or unlisted id.
fn chain_provider_ids() -> Vec<String> {
    let mut ids: Vec<String> = builtin_model_profiles()
        .values()
        .flat_map(|profile| profile.models.iter())
        .flat_map(|rung| rung.providers.iter().copied())
        .map(str::to_owned)
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

#[test]
fn given_builtin_profiles_when_each_provider_id_is_checked_against_the_engine_registry_then_every_id_is_a_builtin_or_an_allow_listed_alias() {
    let unknown: Vec<String> = chain_provider_ids()
        .into_iter()
        .filter(|id| !support::is_known_provider(id))
        .collect();
    assert_eq!(unknown, Vec::<String>::new());
}

#[test]
fn given_the_allow_list_when_compared_with_the_engine_registry_then_no_allow_listed_id_is_a_builtin_so_the_list_cannot_hide_drift() {
    let overlapping: Vec<&str> = support::CHAIN_PROVIDER_ID_ALLOWLIST
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| support::ENGINE_BUILTIN_PROVIDER_IDS.contains(id))
        .collect();
    assert_eq!(overlapping, Vec::<&str>::new());
}
