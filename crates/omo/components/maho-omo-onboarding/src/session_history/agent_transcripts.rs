//! Per-source transcript candidates (upstream `agent-transcripts.ts`).

use std::{collections::BTreeMap, collections::HashSet, sync::atomic::AtomicBool};

use super::constants::{
    MAX_DISCOVERY_OPERATIONS_PER_SOURCE, MAX_METADATA_BYTES_PER_SOURCE,
    MAX_METADATA_OPERATIONS_PER_SOURCE, MAX_METADATA_RECORDS_PER_SOURCE,
    MAX_TRANSCRIPTS_PER_SOURCE,
};
use super::project_paths::{ProjectPathRules, expand_home_path, resolve_path};
use super::session_history_fs::SessionHistoryFs;
use super::transcript_cwd::{CwdGroup, MetadataReadBudget, group_transcripts_by_cwd};
use super::transcript_discovery::{
    TranscriptCandidate, discover_claude_transcripts, discover_codex_transcripts, join,
    select_metadata_transcripts,
};
use super::types::{AgentHome, AgentSessionSource};

#[derive(Clone, Debug)]
pub struct SourceCandidates {
    pub source: AgentSessionSource,
    pub groups: Vec<CwdGroup>,
    pub found: usize,
    pub read: usize,
    pub truncated: bool,
}

/// `CLAUDE_CONFIG_DIR` or `~/.claude`, and `CODEX_HOME` or `~/.codex`, as each CLI resolves its own home.
pub fn default_agent_homes(env: &BTreeMap<String, String>, home_dir: &str) -> Vec<AgentHome> {
    let configured = |key: &str, fallback: &str| {
        let trimmed = env.get(key).map_or("", |v| v.trim());
        if trimmed.is_empty() {
            join(home_dir, fallback)
        } else {
            resolve_path(&expand_home_path(trimmed, home_dir))
        }
    };
    vec![
        AgentHome {
            source: AgentSessionSource::ClaudeAgent,
            home_path: configured("CLAUDE_CONFIG_DIR", ".claude"),
        },
        AgentHome {
            source: AgentSessionSource::Codex,
            home_path: configured("CODEX_HOME", ".codex"),
        },
    ]
}

/// Homes of one source, deduplicated by directory identity.
pub fn unique_homes(
    rules: &ProjectPathRules,
    homes: &[AgentHome],
    source: AgentSessionSource,
) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut unique = Vec::new();
    for home in homes.iter().filter(|home| home.source == source) {
        let home_path = resolve_path(&expand_home_path(&home.home_path, &rules.home_dir));
        let identity = rules.directory_identity(&home_path, None);
        if seen.insert(identity) {
            unique.push(home_path);
        }
    }
    unique
}

/// Splits the per-source budget across `home_count` homes: floor each, remainder to the first homes.
pub fn home_budget(index: usize, home_count: usize) -> i64 {
    let count = i64::try_from(home_count.max(1)).unwrap_or(1);
    let base = MAX_DISCOVERY_OPERATIONS_PER_SOURCE / count;
    let extra = MAX_DISCOVERY_OPERATIONS_PER_SOURCE % count;
    base + i64::from(i64::try_from(index).unwrap_or(i64::MAX) < extra)
}

/// One agent source: shared discovery budget across its homes, newest-first selection, shared metadata budget.
pub fn collect_source_candidates(
    fs: &dyn SessionHistoryFs,
    rules: &ProjectPathRules,
    homes: &[AgentHome],
    source: AgentSessionSource,
) -> SourceCandidates {
    let homes = unique_homes(rules, homes, source);
    let mut truncated = false;
    let mut transcripts: Vec<TranscriptCandidate> = Vec::new();
    for (index, home_path) in homes.iter().enumerate() {
        let budget = home_budget(index, homes.len());
        if budget == 0 {
            truncated = true;
            continue;
        }
        let no_signal: Option<&AtomicBool> = None;
        let discovered = if source == AgentSessionSource::ClaudeAgent {
            discover_claude_transcripts(fs, home_path, budget, no_signal)
        } else {
            discover_codex_transcripts(fs, home_path, budget, no_signal)
        };
        truncated |= discovered.truncated;
        transcripts.extend(discovered.transcripts);
    }
    transcripts.sort_by(|left, right| {
        right
            .mtime_ms
            .cmp(&left.mtime_ms)
            .then_with(|| left.file_path.cmp(&right.file_path))
    });
    if transcripts.len() > MAX_TRANSCRIPTS_PER_SOURCE {
        truncated = true;
    }
    let selected = select_metadata_transcripts(&transcripts);
    let mut metadata_budget = MetadataReadBudget {
        bytes_remaining: MAX_METADATA_BYTES_PER_SOURCE,
        operations_remaining: MAX_METADATA_OPERATIONS_PER_SOURCE,
        records_remaining: MAX_METADATA_RECORDS_PER_SOURCE,
        truncated: false,
    };
    let groups = group_transcripts_by_cwd(fs, &selected, &mut metadata_budget);
    SourceCandidates {
        source,
        groups,
        found: transcripts.len(),
        read: selected.len(),
        truncated: truncated || metadata_budget.truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_homes_honor_env_and_expand_tilde() {
        let mut env = BTreeMap::new();
        let homes = default_agent_homes(&env, "/home/u");
        assert_eq!(homes[0].home_path, join("/home/u", ".claude"));
        assert_eq!(homes[1].home_path, join("/home/u", ".codex"));
        env.insert("CLAUDE_CONFIG_DIR".into(), "  ~/cfg/claude ".into());
        env.insert("CODEX_HOME".into(), "   ".into());
        let homes = default_agent_homes(&env, "/home/u");
        // A configured home is resolved, so on Windows the POSIX fixture gains the cwd's drive.
        assert_eq!(homes[0].home_path, resolve_path("/home/u/cfg/claude"));
        assert_eq!(homes[1].home_path, join("/home/u", ".codex"));
    }

    #[test]
    fn budget_splits_floor_with_remainder_to_first_homes() {
        assert_eq!(home_budget(0, 1), MAX_DISCOVERY_OPERATIONS_PER_SOURCE);
        assert_eq!(
            (home_budget(0, 3), home_budget(1, 3), home_budget(2, 3)),
            (6667, 6667, 6666)
        );
        assert_eq!(home_budget(0, 0), MAX_DISCOVERY_OPERATIONS_PER_SOURCE);
    }
}
