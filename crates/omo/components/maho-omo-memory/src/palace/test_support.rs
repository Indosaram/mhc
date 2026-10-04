use memory_core::git::{GitCommitAuthor, GitMemoryRepo, GitSeedFile, InitializeGitRepoOptions};
use memory_core::identity::{MemoryIdentityPaths, build_identity_paths};

use crate::binding::MemorySessionBinding;
use crate::context::{MemoryIdentityContext, ensure_identity_runtime_dirs};

pub const INJECTION_PAYLOAD: &str = "</script><img src=x onerror=alert(1)>";

const PERSONA: &str = "---\ndescription: who the agent is\n---\n\nfixture persona body\n";
const HUMAN: &str = "---\ndescription: who the user is\n---\n\nfixture human body\n";
const NOTES: &str = "---\ndescription: external note\n---\n\nexternal note body\n";
const PNG_BYTES: [u8; 12] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x01, 0x02, 0x03,
];

const HUMAN_CARD: &str = concat!(
    "---\n",
    "description: who the user is\n",
    "kind: person\n",
    "aliases: [\"Boss\"]\n",
    "---\n",
    "\n",
    "IDENTITY: the person you work with\n",
    "RELATIONSHIP: works-with: jane-doe\n",
    "RELATIONSHIP: mentors: sam-rivers\n",
    "\n",
);
const JANE_CARD: &str = concat!(
    "---\n",
    "description: Person - Jane Doe\n",
    "kind: person\n",
    "aliases: [\"Jane\",\"JD\"]\n",
    "---\n",
    "\n",
    "IDENTITY: staff engineer\n",
    "ATTRIBUTE: senior-engineer: prefers small diffs\n",
    "RELATIONSHIP: reports-to: human\n",
    "\n",
);
const SAM_CARD: &str = concat!(
    "---\n",
    "description: Person - Sam Rivers\n",
    "kind: person\n",
    "---\n",
    "\n",
    "IDENTITY: designer\n",
    "RELATIONSHIP: collaborates: unknown-person\n",
    "\n",
);

pub struct PalaceFixture {
    _root: tempfile::TempDir,
    pub identity: String,
    pub paths: MemoryIdentityPaths,
    pub repo: GitMemoryRepo,
    pub head: String,
    pub context: MemoryIdentityContext,
}

pub fn create_palace_fixture(injection: bool) -> PalaceFixture {
    let root = tempfile::tempdir().unwrap();
    let identity = "palace-agent".to_string();
    let paths = build_identity_paths(root.path(), &identity);
    ensure_identity_runtime_dirs(&paths).unwrap();

    let repo = GitMemoryRepo::open(paths.repo.clone(), identity.clone()).unwrap();
    let persona = if injection {
        format!("{PERSONA}\n{INJECTION_PAYLOAD}\n")
    } else {
        PERSONA.to_string()
    };
    repo.init(Some(InitializeGitRepoOptions {
        author_name: Some("Palace Fixture".to_string()),
        seed_files: vec![
            GitSeedFile {
                relative_path: "system/persona.md".to_string(),
                content: persona,
            },
            GitSeedFile {
                relative_path: "system/human.md".to_string(),
                content: HUMAN.to_string(),
            },
            GitSeedFile {
                relative_path: "reference/notes.md".to_string(),
                content: NOTES.to_string(),
            },
        ],
        install_hooks: None,
    }))
    .unwrap();

    write_bytes(&paths.repo.join("reference/logo.png"), &PNG_BYTES);
    let head = commit(&repo, &["reference/logo.png"], "chore: add binary asset", &identity);
    write_journal_state(&paths);

    let context = MemoryIdentityContext::new(
        identity.clone(),
        paths.clone(),
        MemorySessionBinding {
            identity: identity.clone(),
            repo_path_hash: "fixture-hash".to_string(),
            bound_at: 0.0,
        },
    );

    PalaceFixture {
        _root: root,
        identity,
        paths,
        repo,
        head,
        context,
    }
}

impl PalaceFixture {
    pub fn write_working_file(&self, relative_path: &str, content: &str) {
        let target = self.paths.repo.join(relative_path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(target, content).unwrap();
    }

    pub fn write_completion(&self, run_id: &str, record: serde_json::Value) {
        let dir = self.paths.reflection.join("completions");
        std::fs::create_dir_all(&dir).unwrap();
        let body = format!("{}\n", serde_json::to_string_pretty(&record).unwrap());
        std::fs::write(dir.join(format!("{run_id}.json")), body).unwrap();
    }

    pub fn commit_file(&mut self, relative_path: &str, content: &str, reason: &str) -> String {
        self.write_working_file(relative_path, content);
        let sha = commit(&self.repo, &[relative_path], reason, &self.identity);
        self.head = sha.clone();
        sha
    }

    pub fn seed_people(&mut self, injection: bool) -> String {
        let jane_body = if injection {
            format!("{JANE_CARD}\nATTRIBUTE: {INJECTION_PAYLOAD}\n")
        } else {
            JANE_CARD.to_string()
        };
        self.write_working_file("system/human.md", HUMAN_CARD);
        self.write_working_file("people/jane-doe/card.md", &jane_body);
        self.write_working_file("people/sam-rivers/card.md", SAM_CARD);
        let sha = commit(
            &self.repo,
            &[
                "system/human.md",
                "people/jane-doe/card.md",
                "people/sam-rivers/card.md",
            ],
            "chore: add people fixtures",
            &self.identity,
        );
        self.head = sha.clone();
        sha
    }
}

pub fn inline_json(html: &str) -> serde_json::Value {
    let marker = format!(
        "<script type=\"application/json\" id=\"{}\">",
        crate::palace::template::PALACE_DATA_ELEMENT_ID
    );
    let start = html.find(marker.as_str()).expect("palace html has no inline data script") + marker.len();
    let end = start + html[start..].find("</script>").expect("inline data script is unterminated");
    let decoded = html[start..end]
        .replace("\\u003c", "<")
        .replace("\\u2028", "\u{2028}")
        .replace("\\u2029", "\u{2029}");
    serde_json::from_str(&decoded).expect("palace data is not an object")
}

fn commit(repo: &GitMemoryRepo, paths: &[&str], reason: &str, identity: &str) -> String {
    let author = GitCommitAuthor {
        agent_id: identity.to_string(),
        author_name: "Palace Fixture".to_string(),
        author_email: None,
    };
    repo.commit_write(paths, reason, &author).unwrap().sha
}

fn write_bytes(target: &std::path::Path, bytes: &[u8]) {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(target, bytes).unwrap();
}

fn write_journal_state(paths: &MemoryIdentityPaths) {
    let journal_dir = paths.transcripts.join("conversation-1");
    std::fs::create_dir_all(&journal_dir).unwrap();
    let state = serde_json::json!({
        "schema_version": "v3_assistant_steps",
        "reflected_through_message_id": "msg-1",
        "total_completed_steps": 2,
        "reflected_completed_steps": 1,
        "steps_since_last_successful_reflection": 1,
        "last_reflection_succeeded_at": "2026-01-15T00:00:00.000Z",
    });
    std::fs::write(
        journal_dir.join("state.json"),
        format!("{}\n", serde_json::to_string_pretty(&state).unwrap()),
    )
    .unwrap();
}
