//! `mhc init-deep` (spec §5).

use crate::generate::{Outcome, Runner, generate};
use crate::plan::{Settings, plan, table};
use std::path::{Path, PathBuf};

pub const USAGE: &str = "Usage: mhc init-deep [--root DIR] [--plan] [--update] [--dry-run] [--json]
                     [--model MODEL] [--max-depth N] [--max-dirs N] [--min-files N]

Writes AGENTS.md context files for the repository (the git top-level of DIR, default the current
directory): one at the root and one in each significant directory, deepest first, each written by a
model run that may read the code. --plan only prints the chosen directories; --update revises
existing files instead of keeping them; --dry-run prints the files instead of writing them.";

/// Parsed arguments.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Args {
    pub root: Option<PathBuf>,
    pub plan_only: bool,
    pub update: bool,
    pub dry_run: bool,
    pub json: bool,
    pub model: Option<String>,
    pub max_depth: Option<usize>,
    pub max_dirs: Option<usize>,
    pub min_files: Option<usize>,
    pub help: bool,
}

pub fn parse(args: &[String]) -> Result<Args, String> {
    let mut out = Args::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = |name: &str| {
            iter.next()
                .cloned()
                .ok_or_else(|| format!("{name} requires a value"))
        };
        let number = |name: &str, text: String| {
            text.parse::<usize>()
                .map_err(|_| format!("{name} needs a whole number, got \"{text}\""))
        };
        match arg.as_str() {
            "--root" => out.root = Some(PathBuf::from(value("--root")?)),
            "--plan" => out.plan_only = true,
            "--update" => out.update = true,
            "--dry-run" => out.dry_run = true,
            "--json" => out.json = true,
            "--model" => out.model = Some(value("--model")?),
            "--max-depth" => out.max_depth = Some(number("--max-depth", value("--max-depth")?)?),
            "--max-dirs" => out.max_dirs = Some(number("--max-dirs", value("--max-dirs")?)?.max(1)),
            "--min-files" => {
                out.min_files = Some(number("--min-files", value("--min-files")?)?.max(1));
            }
            "--help" | "-h" => out.help = true,
            other => return Err(format!("unknown argument \"{other}\"")),
        }
    }
    Ok(out)
}

/// Output of one run.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

/// Runs `mhc init-deep <args>` from `cwd` with config `settings`; `make_runner` builds the model
/// runner for the given `--model`.
pub fn run(
    args: &[String],
    cwd: &Path,
    settings: Settings,
    make_runner: &mut dyn FnMut(Option<String>) -> Box<dyn Runner>,
) -> Output {
    let args = match parse(args) {
        Ok(args) => args,
        Err(message) => {
            return Output {
                stderr: format!("mhc init-deep: {message}\n{USAGE}"),
                code: 2,
                ..Output::default()
            };
        }
    };
    if args.help {
        return Output {
            stdout: USAGE.into(),
            ..Output::default()
        };
    }
    let start = args
        .root
        .as_ref()
        .map_or_else(|| cwd.to_path_buf(), |r| cwd.join(r));
    if !start.is_dir() {
        return Output {
            stderr: format!("mhc init-deep: not a directory: {}", start.display()),
            code: 2,
            ..Output::default()
        };
    }
    let mut settings = settings;
    if let Some(v) = args.max_depth {
        settings.max_depth = v;
    }
    if let Some(v) = args.max_dirs {
        settings.max_dirs = v;
    }
    if let Some(v) = args.min_files {
        settings.min_files = v;
    }
    let root = crate::analyze::project_root(&start);
    let analysis = crate::analyze::analyze(&root);
    let root_for_read = root.clone();
    let plan = plan(&analysis, &settings, args.update, &move |rel, name| {
        let dir = if rel == "." {
            root_for_read.clone()
        } else {
            rel.split('/').fold(root_for_read.clone(), |p, c| p.join(c))
        };
        std::fs::read_to_string(dir.join(name)).ok()
    });
    if args.plan_only {
        return Output {
            stdout: if args.json {
                serde_json::to_string_pretty(&plan).unwrap_or_default()
            } else {
                table(&plan)
            },
            ..Output::default()
        };
    }
    let mut runner = make_runner(args.model.clone());
    let report = generate(&plan, runner.as_mut(), args.dry_run);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    for (path, outcome) in &report.outcomes {
        match outcome {
            Outcome::Written(file) => stdout.push(format!("wrote {}", file.display())),
            Outcome::Preview(file, content) => {
                stdout.push(format!("--- {}\n{}", file.display(), content.trim_end()));
            }
            Outcome::Kept => stdout.push(format!("kept {path} (existing context file)")),
            Outcome::Failed(error) => stderr.push(format!("failed {path}: {error}")),
        }
    }
    stdout.push(report.summary_line());
    Output {
        stdout: stdout.join("\n"),
        stderr: stderr.join("\n"),
        code: i32::from(report.failed() > 0),
    }
}
