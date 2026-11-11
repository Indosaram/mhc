//! Subprocess QA for the MIT profile's memory command (`mhc memory ...`), the only memory surface the
//! default build ships.
//!
//! Every other memory test in this workspace exercises a library function directly, so a break in the
//! wiring between `run_memory_entry` and the store would pass all of them. These tests only ever talk
//! to the built binary and to files it creates.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Home {
    root: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().expect("home"),
        }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn agent_dir(&self) -> PathBuf {
        self.path().join("agent")
    }

    /// Runs the shipped binary with an environment that cannot reach the developer's own memory store:
    /// `HOME`, the agent directory and the mnemopi data directory all point inside the tempdir, and the
    /// ambient environment is cleared so no `MNEMOPI_DATA_DIR` or `CODING_AGENT_DIR` leaks in.
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_mhc"))
            .env_clear()
            .env("HOME", self.path())
            .env("CODING_AGENT_DIR", self.agent_dir())
            .env("MNEMOPI_DATA_DIR", self.path().join("mnemopi"))
            .env("PATH", "/usr/bin:/bin")
            .current_dir(self.path())
            .args(args)
            .output()
            .expect("mhc")
    }
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout")
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr")
}

#[test]
fn the_memory_command_is_reachable_in_the_default_build() {
    let home = Home::new();
    let output = home.run(&["memory", "--help"]);
    assert!(
        output.status.success(),
        "status {:?} stderr {}",
        output.status.code(),
        stderr_of(&output)
    );
    let text = stdout_of(&output);
    for command in ["status", "search", "remember", "import-legacy"] {
        assert!(text.contains(command), "help omits {command}: {text}");
    }
}

#[test]
fn a_remembered_memory_is_found_by_a_later_search() {
    let home = Home::new();
    let remembered = home.run(&["memory", "remember", "the deploy key lives in the vault"]);
    assert!(
        remembered.status.success(),
        "remember failed: {}",
        stderr_of(&remembered)
    );
    assert!(stdout_of(&remembered).contains("Stored memory"));

    let found = home.run(&["memory", "search", "deploy key vault"]);
    assert!(
        found.status.success(),
        "search failed: {}",
        stderr_of(&found)
    );
    let text = stdout_of(&found);
    assert!(
        text.contains("vault"),
        "the memory just stored is not searchable: {text}"
    );
    assert!(!text.contains("No relevant memories found."));
}

#[test]
fn a_store_starts_empty_so_search_finds_nothing_before_a_remember() {
    let home = Home::new();
    let output = home.run(&["memory", "search", "anything at all"]);
    assert!(output.status.success(), "stderr {}", stderr_of(&output));
    assert!(
        stdout_of(&output).contains("No relevant memories found."),
        "a fresh store must not invent results: {}",
        stdout_of(&output)
    );
}

#[test]
fn the_store_survives_between_separate_processes() {
    let home = Home::new();
    home.run(&["memory", "remember", "the release tag is cut from main"]);
    let status = home.run(&["memory", "status"]);
    assert!(status.status.success(), "stderr {}", stderr_of(&status));
    let text = stdout_of(&status);
    assert!(!text.trim().is_empty(), "status printed nothing");
}

#[test]
fn a_remember_without_text_is_refused_rather_than_storing_an_empty_memory() {
    let home = Home::new();
    let output = home.run(&["memory", "remember"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr_of(&output).contains("usage: mhc memory remember"),
        "stderr {}",
        stderr_of(&output)
    );
}

#[test]
fn a_search_without_a_query_is_refused() {
    let home = Home::new();
    let output = home.run(&["memory", "search"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr_of(&output).contains("usage: mhc memory search"),
        "stderr {}",
        stderr_of(&output)
    );
}
