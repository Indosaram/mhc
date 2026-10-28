//! Writing the planned files through a model runner, deepest first (spec §5).

use crate::plan::{Action, Plan};
use crate::prompt::{clean_answer, file_prompt, summary};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Runs one prompt in one directory and returns the model's answer.
pub trait Runner {
    fn run(&mut self, dir: &Path, prompt: &str) -> Result<String, String>;
}

/// Outcome of one target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Written(PathBuf),
    /// `--dry-run`: the content that would be written.
    Preview(PathBuf, String),
    Kept,
    Failed(String),
}

/// Outcomes in processing order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub outcomes: Vec<(String, Outcome)>,
}

impl Report {
    pub fn failed(&self) -> usize {
        self.count(|o| matches!(o, Outcome::Failed(_)))
    }
    fn count(&self, f: impl Fn(&Outcome) -> bool) -> usize {
        self.outcomes.iter().filter(|(_, o)| f(o)).count()
    }
    /// `context files: N written, M kept, K failed`.
    pub fn summary_line(&self) -> String {
        format!(
            "context files: {} written, {} kept, {} failed",
            self.count(|o| matches!(o, Outcome::Written(_) | Outcome::Preview(..))),
            self.count(|o| matches!(o, Outcome::Kept)),
            self.failed()
        )
    }
}

fn dir_of(root: &Path, rel: &str) -> PathBuf {
    if rel == "." {
        root.to_path_buf()
    } else {
        rel.split('/').fold(root.to_path_buf(), |p, c| p.join(c))
    }
}

fn existing_summary(dir: &Path) -> Option<String> {
    crate::analyze::CONTEXT_FILES
        .iter()
        .find_map(|name| std::fs::read_to_string(dir.join(name)).ok())
        .map(|text| summary(&text))
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let temp = path.with_file_name(format!(".AGENTS.md.tmp-{}", std::process::id()));
    std::fs::write(&temp, text)?;
    std::fs::rename(&temp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

/// Writes every `create`/`update` target of `plan` (deepest first, as planned).
pub fn generate(plan: &Plan, runner: &mut dyn Runner, dry_run: bool) -> Report {
    let root = PathBuf::from(&plan.root);
    let mut summaries: BTreeMap<String, String> = BTreeMap::new();
    let mut report = Report::default();
    for target in &plan.targets {
        let dir = dir_of(&root, &target.path);
        if target.action == Action::Keep {
            if let Some(text) = existing_summary(&dir) {
                summaries.insert(target.path.clone(), text);
            }
            report.outcomes.push((target.path.clone(), Outcome::Kept));
            continue;
        }
        let children: Vec<(String, String)> = target
            .children
            .iter()
            .filter_map(|c| summaries.get(c).map(|s| (c.clone(), s.clone())))
            .collect();
        let prompt = file_prompt(target, &children);
        let outcome = match runner.run(&dir, &prompt) {
            Ok(answer) => {
                let content = clean_answer(&answer);
                if content.is_empty() {
                    Outcome::Failed("the model returned no text".into())
                } else {
                    summaries.insert(target.path.clone(), summary(&content));
                    let path = dir.join("AGENTS.md");
                    if dry_run {
                        Outcome::Preview(path, content)
                    } else {
                        match write_atomic(&path, &content) {
                            Ok(()) => Outcome::Written(path),
                            Err(error) => Outcome::Failed(error.to_string()),
                        }
                    }
                }
            }
            Err(error) => Outcome::Failed(error),
        };
        report.outcomes.push((target.path.clone(), outcome));
    }
    report
}

/// A [`Runner`] that runs the current executable as `mhc -p` in the target directory.
pub struct ChildRunner {
    pub executable: PathBuf,
    pub model: Option<String>,
    pub extra_args: Vec<String>,
    pub timeout: std::time::Duration,
}

/// Read-only tools offered to the child.
pub const CHILD_TOOLS: &str = "read,grep,find,ls";

impl ChildRunner {
    /// The child's argument list for `prompt`.
    pub fn args(&self, prompt: &str) -> Vec<String> {
        let mut args = vec![
            "-p".to_owned(),
            "--no-session".to_owned(),
            "--tools".to_owned(),
            CHILD_TOOLS.to_owned(),
        ];
        if let Some(model) = &self.model {
            args.push("--model".into());
            args.push(model.clone());
        }
        args.extend(self.extra_args.iter().cloned());
        args.push(prompt.to_owned());
        args
    }
}

impl Runner for ChildRunner {
    fn run(&mut self, dir: &Path, prompt: &str) -> Result<String, String> {
        let mut child = std::process::Command::new(&self.executable)
            .args(self.args(prompt))
            .current_dir(dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", self.executable.display()))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let out_reader = std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut s) = stdout {
                let _ = std::io::Read::read_to_string(&mut s, &mut text);
            }
            text
        });
        let err_reader = std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut s) = stderr {
                let _ = std::io::Read::read_to_string(&mut s, &mut text);
            }
            text
        });
        let started = std::time::Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                break status;
            }
            if started.elapsed() > self.timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {}s", self.timeout.as_secs()));
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        };
        let out = out_reader.join().unwrap_or_default();
        let err = err_reader.join().unwrap_or_default();
        if status.success() {
            Ok(out)
        } else {
            let tail: String = err
                .lines()
                .rev()
                .take(5)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            Err(format!("model run exited with {status}: {tail}"))
        }
    }
}
