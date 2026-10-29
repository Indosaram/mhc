//! `team/runtime-config.test.ts`

use pretty_assertions::assert_eq;
use regex::Regex;

use crate::team::registry::TeamSpecSource;
use crate::team::runtime_config::{TeamCoreConfig, TeamCoreSpecSource, to_team_core_config, to_team_core_spec_source};
use crate::team::runtime_fakes::{TeamBoundsOverrides, team_bounds};

/// Reads a field's rendered value from the config's Debug output, unwrapping `Some(..)` and string
/// quotes so the assertion is independent of whether team-core stores the field as optional.
fn field(config: &TeamCoreConfig, name: &str) -> String {
    let rendered = format!("{config:?}");
    let pattern = Regex::new(&format!(r#"(?:^|[ {{,]){}: (?:Some\()?("[^"]*"|[^,}}()\s]+)"#, regex::escape(name)))
        .expect("valid field regex");
    let captures = pattern
        .captures(&rendered)
        .unwrap_or_else(|| panic!("field {name} not found in {rendered}"));
    captures[1].trim_matches('"').to_string()
}

#[test]
fn given_omo_task_team_bounds_when_converted_then_the_team_core_config_carries_the_bounds_and_base_dir() {
    // given
    let settings = team_bounds(TeamBoundsOverrides {
        max_members: Some(5),
        max_parallel_members: Some(2),
        max_wall_clock_minutes: Some(30),
    });

    // when
    let config = to_team_core_config(&settings, "/tmp/state/teams").expect("config parses");

    // then
    assert_eq!(field(&config, "base_dir"), "/tmp/state/teams");
    assert_eq!(field(&config, "max_members"), "5");
    assert_eq!(field(&config, "max_parallel_members"), "2");
    assert_eq!(field(&config, "max_wall_clock_minutes"), "30");
}

#[test]
fn given_defaults_when_converted_then_transport_fields_keep_team_core_defaults() {
    // given
    let settings = team_bounds(TeamBoundsOverrides::default());

    // when
    let config = to_team_core_config(&settings, "/tmp/base").expect("config parses");

    // then
    assert_eq!(field(&config, "enabled"), "true");
    assert_eq!(field(&config, "tmux_visualization"), "false");
    assert_eq!(field(&config, "max_members"), "8");
    assert_eq!(field(&config, "max_parallel_members"), "4");
    assert_eq!(field(&config, "max_wall_clock_minutes"), "120");
}

#[test]
fn given_a_project_source_when_mapped_then_it_stays_project() {
    // when / then
    assert!(matches!(
        to_team_core_spec_source(TeamSpecSource::Project),
        TeamCoreSpecSource::Project
    ));
}

#[test]
fn given_an_omo_json_source_when_mapped_then_it_maps_to_the_user_slot() {
    // when / then
    assert!(matches!(
        to_team_core_spec_source(TeamSpecSource::OmoJson),
        TeamCoreSpecSource::User
    ));
}
