//! Projects senpi team bounds onto a team-core config.

use serde_json::json;
use team_core::TeamModeConfig;
use team_core::types::SpecSource;

use crate::team::registry::TeamSpecSource;

/// The team-core config shape senpi drives.
pub type TeamCoreConfig = TeamModeConfig;
/// The team-core spec-provenance shape.
pub type TeamCoreSpecSource = SpecSource;

/// The omo `task.team` bounds senpi maps onto team-core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeamTaskBounds {
    pub max_members: u64,
    pub max_parallel_members: u64,
    pub max_wall_clock_minutes: u64,
}

/// Projects the omo `task.team` bounds onto a team-core config, pinning `base_dir` to the senpi team
/// storage root so team-core writes runtime state under the senpi state dir instead of `~/.omo`.
/// Fields team-core needs but senpi never drives (tmux + mailbox transport) are held at team-core's
/// own defaults so runtime-state creation and the mailbox APIs stay well-formed.
pub fn to_team_core_config(team: &TeamTaskBounds, base_dir: &str) -> Result<TeamCoreConfig, String> {
    let raw = json!({
        "enabled": true,
        "tmux_visualization": false,
        "max_messages_per_run": 10000,
        "max_member_turns": 500,
        "message_payload_max_bytes": 32768,
        "recipient_unread_max_bytes": 262144,
        "mailbox_poll_interval_ms": 3000,
        "base_dir": base_dir,
        "max_members": team.max_members,
        "max_parallel_members": team.max_parallel_members,
        "max_wall_clock_minutes": team.max_wall_clock_minutes,
    });
    TeamModeConfig::parse(&raw)
}

/// team-core records spec provenance as "project" | "user"; senpi's registry distinguishes project
/// spec files from `omo.json` inline specs. The inline specs land in the non-project ("user") slot.
pub fn to_team_core_spec_source(source: TeamSpecSource) -> TeamCoreSpecSource {
    match source {
        TeamSpecSource::Project => SpecSource::Project,
        TeamSpecSource::OmoJson => SpecSource::User,
    }
}
