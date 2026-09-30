//! Port of senpi `packages/coding-agent/src/core/dynamic-prompt/workstation.ts`.

use std::sync::LazyLock;

/// `WorkstationFacts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkstationFacts {
    pub os_line: String,
    pub kernel: String,
    pub arch: String,
    pub cpu: Option<String>,
    pub gpu: Option<String>,
    pub terminal: Option<String>,
}

/// `WorkstationDialect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WorkstationDialect {
    #[default]
    Default,
    Claude,
    Codex,
    Kimi,
}

fn cpu_model() -> Option<String> {
    if cfg!(target_os = "linux")
        && let Ok(cpu_info) = std::fs::read_to_string("/proc/cpuinfo")
    {
        for line in cpu_info.lines() {
            if let Some(rest) = line.strip_prefix("model name")
                && let Some((_, value)) = rest.split_once(':')
            {
                let value = value.trim();
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    if cfg!(target_os = "macos") {
        let output = std::process::Command::new("sysctl").args(["-n", "machdep.cpu.brand_string"]).output().ok()?;
        let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

fn terminal_name() -> Option<String> {
    let program = std::env::var("TERM_PROGRAM").ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty());
    if let Some(program) = program {
        let version = std::env::var("TERM_PROGRAM_VERSION").ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty());
        return Some(match version {
            Some(version) => format!("{program} {version}"),
            None => program,
        });
    }
    std::env::var("TERM").ok().map(|value| value.trim().to_string()).filter(|value| !value.is_empty())
}

/// `os.platform()` spelled the way Node spells it.
fn os_platform() -> String {
    match std::env::consts::OS {
        "macos" => "darwin".to_string(),
        other => other.to_string(),
    }
}

/// `os.arch()` spelled the way Node spells it.
fn os_arch() -> String {
    match std::env::consts::ARCH {
        "x86_64" => "x64".to_string(),
        "aarch64" => "arm64".to_string(),
        "x86" => "ia32".to_string(),
        "arm" => "arm".to_string(),
        other => other.to_string(),
    }
}

/// `os.type()` spelled the way Node spells it.
fn os_type() -> &'static str {
    match std::env::consts::OS {
        "macos" => "Darwin",
        "windows" => "Windows_NT",
        "linux" => "Linux",
        _ => "Unknown",
    }
}

/// `os.release()`: the kernel release, from `/proc/sys/kernel/osrelease` on Linux.
fn os_release() -> String {
    if cfg!(target_os = "linux")
        && let Ok(release) = std::fs::read_to_string("/proc/sys/kernel/osrelease")
    {
        return release.trim().to_string();
    }
    if let Ok(output) = std::process::Command::new("uname").arg("-r").output() {
        let release = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !release.is_empty() {
            return release;
        }
    }
    String::new()
}

static CACHED_FACTS: LazyLock<WorkstationFacts> = LazyLock::new(collect_workstation_facts_uncached);

fn collect_workstation_facts_uncached() -> WorkstationFacts {
    let cpu = cpu_model();
    let cores = std::thread::available_parallelism().map(|cores| cores.get()).unwrap_or(0);
    let gpu = if cfg!(target_os = "macos")
        && std::env::consts::ARCH == "aarch64"
        && cpu.as_deref().is_some_and(|cpu| cpu.starts_with("Apple "))
    {
        cpu.clone()
    } else {
        None
    };
    let terminal = terminal_name();
    WorkstationFacts {
        os_line: os_platform(),
        kernel: format!("{} {}", os_type(), os_release()).trim().to_string(),
        arch: os_arch(),
        cpu: cpu.map(|cpu| format!("{cpu} ({cores} cores)")),
        gpu,
        terminal,
    }
}

/// `collectWorkstationFacts`: host facts collected once per process.
pub fn collect_workstation_facts() -> &'static WorkstationFacts {
    &CACHED_FACTS
}

/// `executorPhrase`.
pub fn executor_phrase(selected_tools: &[String]) -> String {
    let executors: Vec<&str> =
        ["bash", "eval"].iter().copied().filter(|name| selected_tools.iter().any(|tool| tool.as_str() == *name)).collect();
    if executors.is_empty() {
        return "Everything you run executes".to_string();
    }
    let names = executors.iter().map(|name| format!("`{name}`")).collect::<Vec<_>>().join(" and ");
    let verb = if executors.len() == 1 { "executes" } else { "execute" };
    format!("{names} {verb}")
}

fn instruction(dialect: WorkstationDialect, executors: &str) -> String {
    match dialect {
        WorkstationDialect::Claude => format!(
            "<execution_context>\n{executors} on THIS workstation. Match commands, paths, package managers, and parallel pool sizes to it. Code you write may target any machine; code you run always runs here.\n</execution_context>"
        ),
        WorkstationDialect::Codex => format!(
            "{executors} on this workstation; match commands, paths, and parallelism to it. Written code may target other platforms — executed code runs here."
        ),
        WorkstationDialect::Kimi => format!(
            "{executors} on this workstation — choose commands, paths, and parallelism that fit it. When writing code for another target platform, keep the code portable; only the execution happens here."
        ),
        WorkstationDialect::Default => format!(
            "**EXECUTION HAPPENS HERE.** {executors} on THIS machine — you MUST match commands, paths, package managers, and parallel pool sizes to the workstation above. Code you WRITE may target a different machine; anything you RUN executes HERE."
        ),
    }
}

/// `BuildWorkstationSectionOptions`.
#[derive(Debug, Clone, Default)]
pub struct BuildWorkstationSectionOptions {
    pub selected_tools: Vec<String>,
    pub dialect: Option<WorkstationDialect>,
    pub facts: Option<WorkstationFacts>,
}

/// `buildWorkstationSection`.
pub fn build_workstation_section(options: &BuildWorkstationSectionOptions) -> String {
    let facts = options.facts.clone().unwrap_or_else(|| collect_workstation_facts().clone());
    let dialect = options.dialect.unwrap_or_default();
    let mut lines: Vec<String> = vec![format!("- OS: {} (kernel {})", facts.os_line, facts.kernel), format!("- Arch: {}", facts.arch)];
    if let Some(cpu) = &facts.cpu {
        lines.push(format!("- CPU: {cpu}"));
    }
    if let Some(gpu) = &facts.gpu {
        lines.push(format!("- GPU: {gpu}"));
    }
    if let Some(terminal) = &facts.terminal {
        lines.push(format!("- Terminal: {terminal}"));
    }
    let instruction = instruction(dialect, &executor_phrase(&options.selected_tools));
    format!("<workstation>\n{}\n</workstation>\n{instruction}", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> WorkstationFacts {
        WorkstationFacts {
            os_line: "linux".to_string(),
            kernel: "Linux 6.8.0-45-generic".to_string(),
            arch: "x64".to_string(),
            cpu: Some("AMD Ryzen 5 5600X 6-Core Processor (12 cores)".to_string()),
            gpu: None,
            terminal: Some("xterm-256color".to_string()),
        }
    }

    fn options(dialect: WorkstationDialect, tools: &[&str]) -> BuildWorkstationSectionOptions {
        BuildWorkstationSectionOptions {
            selected_tools: tools.iter().map(|tool| (*tool).to_string()).collect(),
            dialect: Some(dialect),
            facts: Some(facts()),
        }
    }

    #[test]
    fn the_fact_block_omits_absent_optional_facts() {
        let section = build_workstation_section(&options(WorkstationDialect::Default, &["bash"]));
        assert!(section.starts_with("<workstation>\n- OS: linux (kernel Linux 6.8.0-45-generic)\n- Arch: x64\n- CPU: AMD Ryzen 5 5600X 6-Core Processor (12 cores)\n- Terminal: xterm-256color\n</workstation>\n"));
        assert!(!section.contains("- GPU:"));
    }

    #[test]
    fn the_executor_phrase_names_the_tools_that_are_selected() {
        assert_eq!(executor_phrase(&["bash".to_string(), "eval".to_string()]), "`bash` and `eval` execute");
        assert_eq!(executor_phrase(&["bash".to_string()]), "`bash` executes");
        assert_eq!(executor_phrase(&["read".to_string()]), "Everything you run executes");
    }

    #[test]
    fn every_dialect_keeps_its_own_wording() {
        let default_section = build_workstation_section(&options(WorkstationDialect::Default, &["bash", "eval"]));
        assert!(default_section.ends_with("**EXECUTION HAPPENS HERE.** `bash` and `eval` execute on THIS machine — you MUST match commands, paths, package managers, and parallel pool sizes to the workstation above. Code you WRITE may target a different machine; anything you RUN executes HERE."));

        let claude = build_workstation_section(&options(WorkstationDialect::Claude, &["bash"]));
        assert!(claude.ends_with("<execution_context>\n`bash` executes on THIS workstation. Match commands, paths, package managers, and parallel pool sizes to it. Code you write may target any machine; code you run always runs here.\n</execution_context>"));

        let codex = build_workstation_section(&options(WorkstationDialect::Codex, &["bash"]));
        assert!(codex.ends_with("`bash` executes on this workstation; match commands, paths, and parallelism to it. Written code may target other platforms — executed code runs here."));

        let kimi = build_workstation_section(&options(WorkstationDialect::Kimi, &["eval"]));
        assert!(kimi.ends_with("`eval` executes on this workstation — choose commands, paths, and parallelism that fit it. When writing code for another target platform, keep the code portable; only the execution happens here."));
    }

    #[test]
    fn collected_facts_are_cached_and_shaped_like_node() {
        let facts = collect_workstation_facts();
        assert_eq!(facts, collect_workstation_facts());
        assert!(!facts.os_line.is_empty());
        assert!(!facts.kernel.is_empty());
        assert!(!facts.arch.is_empty());
    }
}
