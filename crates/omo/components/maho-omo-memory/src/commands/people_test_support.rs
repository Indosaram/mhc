//! People command test fixtures.
//! Port of `components/memory/commands/people.test-support.ts` at pin 77f3067f1.

use memory_core::{
    git::{GitMemoryRepo, GitSeedFile},
    people::format::PeopleLimits,
};

use super::test_support::{TEST_IDENTITY, seed, seeded_repo, temp_identity};
use super::types::MemoryCommandIdentity;

pub const LIMITS: PeopleLimits = PeopleLimits { max_entries: 40, max_entry_chars: 200 };

const HUMAN_CARD: &str = "---\ndescription: who the user is\nkind: person\naliases: [\"Boss\"]\n---\n\nIDENTITY: the person you work with\nRELATIONSHIP: works-with: jane-doe\nRELATIONSHIP: mentors: sam-rivers\n\n";

pub const JANE_CARD: &str = "---\ndescription: Person - Jane Doe\nkind: person\naliases: [\"Jane\",\"JD\"]\n---\n\nIDENTITY: staff engineer\nATTRIBUTE: prefers small diffs\nRELATIONSHIP: reports-to: human\n\n";

const SAM_CARD: &str = "---\ndescription: Person - Sam Rivers\nkind: person\n---\n\nIDENTITY: designer\nRELATIONSHIP: collaborates: unknown-person\n\n";

const BLANK_CARD: &str = "---\ndescription: Person - Nia Blank\nkind: person\n---\n";

fn observations_file(count: usize) -> String {
    let mut lines = vec![
        "---".to_owned(),
        "description: Observations - Jane Doe".to_owned(),
        "---".to_owned(),
        String::new(),
        "## Explicit".to_owned(),
        String::new(),
    ];
    for index in 0..count {
        lines.push(format!(
            "- [2026-03-{:02}] explicit note {} <!-- src: s-{} -->",
            index + 1,
            index + 1,
            index + 1
        ));
    }
    lines.push(String::new());
    lines.push("## Inductive".to_owned());
    lines.push(String::new());
    lines.push("- [2026-01-05] tends to review in the morning <!-- pattern: cadence; confidence: low -->".to_owned());
    format!("{}\n", lines.join("\n"))
}

pub fn people_fixture(observation_count: usize) -> (tempfile::TempDir, MemoryCommandIdentity) {
    let (root, identity) = temp_identity();
    let mut seeds: Vec<GitSeedFile> = vec![
        seed("system/human.md", HUMAN_CARD),
        seed("people/jane-doe/card.md", JANE_CARD),
        seed("people/sam-rivers/card.md", SAM_CARD),
        seed("people/nia-blank/card.md", BLANK_CARD),
    ];
    if observation_count > 0 {
        seeds.push(seed(
            "people/jane-doe/observations.md",
            &observations_file(observation_count),
        ));
    }
    seeded_repo(&identity, seeds);
    (root, identity)
}

/// Kept for parity with the upstream fixture's exported identity constant.
pub fn fixture_identity() -> &'static str {
    TEST_IDENTITY
}

/// Opens the fixture repository for a derived identity.
pub fn fixture_repo(identity: &MemoryCommandIdentity) -> GitMemoryRepo {
    super::test_support::open_repo(identity)
}
