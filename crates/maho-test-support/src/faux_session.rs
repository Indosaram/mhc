use std::path::PathBuf;
use std::process::Command;
use serde_json::Value;

use crate::faux::FauxScript;

pub struct FauxSession {
    pub scenario: String,
    pub omo: bool,
    pub script: FauxScript,
}

impl FauxSession {
    pub fn new(script: FauxScript) -> Self {
        Self {
            scenario: script.name.clone(),
            omo: false,
            script,
        }
    }

    pub fn with_extension(mut self, _ext: impl Into<String>) -> Self {
        self.omo = true;
        self
    }

    pub fn run_and_serialize(&self) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
        run_faux_scenario(&self.scenario, self.omo)
    }
}

pub fn run_faux_scenario(scenario: &str, omo: bool) -> Result<Value, Box<dyn std::error::Error + Send + Sync>> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp_out = std::env::temp_dir().join(format!("maho-faux-{}-{}.json", scenario, now));
    let repo_root = find_repo_root();
    let script_path = repo_root.join("tools/golden/faux-harness.mjs");

    let mut cmd = Command::new("bun");
    cmd.arg(&script_path)
        .arg("--scenario")
        .arg(scenario)
        .arg("--out")
        .arg(&temp_out);
    if omo {
        cmd.arg("--omo");
    }

    let status = cmd.status()?;
    if !status.success() {
        return Err(format!("faux-harness generator failed with exit status {status}").into());
    }

    let data = std::fs::read_to_string(&temp_out)?;
    let _ = std::fs::remove_file(&temp_out);
    let json: Value = serde_json::from_str(&data)?;
    Ok(json)
}

fn find_repo_root() -> PathBuf {
    let mut dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    while !dir.join("tools/golden/faux-harness.mjs").exists() {
        if let Some(parent) = dir.parent() {
            dir = parent.to_path_buf();
        } else {
            return PathBuf::from("/home/indo/T9-Mac/maho-code");
        }
    }
    dir
}
