//! MIT profile (`--no-default-features`): long-term memory from `maho-memory` (a port of the MIT
//! mnemopi engine). Mounts `<builtin:memory>` (the `retain`, `recall`, `memory_get` and
//! `memory_forget` tools, the memory prompt section, first-turn auto-recall, `/memory`), serves the
//! memory MCP server (`mhc memory mcp`, `--memory-mcp`, and the legacy `--omo-memory-mcp` entry),
//! and runs `mhc memory ...` from the shell, including the import of the legacy Markdown store.
//! The `omo` build keeps its own memory runtime.
use maho_memory::banks::{DEFAULT_BANK, Scoping};
use maho_memory::legacy::{ImportOptions, default_root, import};
use maho_memory::{MemoryContext, MemoryExtension, ServerConfig};
use std::path::{Path, PathBuf};

/// Entry flags that start the memory MCP stdio server.
pub const MCP_ENTRY_FLAGS: &[&str] = &["--memory-mcp", "--omo-memory-mcp"];

const USAGE: &str = "Usage: mhc memory <command>

Commands:
  status                         Banks for this directory and their sizes
  search <query>                 Recall memories for this directory
  remember [--global] <text>     Store a memory in this project's bank (or the global bank)
  import-legacy [--from <dir>] [--dry-run] [--include-people] [--json]
                                 Import the legacy Markdown memory store (default:
                                 $MAHO_MEMORY_HOME or ~/.omo/memory) into the global bank
  mcp [--bank <name>]            Serve the memory MCP server on stdio

Data directory: $MNEMOPI_DATA_DIR, else <agent dir>/memories/mnemopi.
Scoping: $MAHO_MEMORY_SCOPING = per-project-tagged (default) | per-project | global.
";

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// `$MNEMOPI_DATA_DIR`, else `<agent dir>/memories/mnemopi`.
pub fn data_dir() -> PathBuf {
    non_empty_env("MNEMOPI_DATA_DIR").map_or_else(
        || {
            PathBuf::from(maho_core::config::get_agent_dir())
                .join("memories")
                .join("mnemopi")
        },
        PathBuf::from,
    )
}

/// The legacy Markdown store: `$MAHO_MEMORY_HOME`, else `~/.omo/memory`.
pub fn legacy_root() -> PathBuf {
    default_root(
        non_empty_env("MAHO_MEMORY_HOME").as_deref(),
        &maho_core::config::home_dir_path(),
    )
}

fn scoping() -> Scoping {
    non_empty_env("MAHO_MEMORY_SCOPING")
        .and_then(|value| Scoping::parse(&value))
        .unwrap_or(Scoping::PerProjectTagged)
}

/// The memory extension for this process.
pub fn extension() -> MemoryExtension {
    MemoryExtension {
        scoping: scoping(),
        ..MemoryExtension::new(data_dir(), legacy_root())
    }
}

fn mcp_config() -> ServerConfig {
    let mut config = ServerConfig::new(data_dir());
    if let Some(bank) = non_empty_env("MNEMOPI_MCP_BANK") {
        config.bank = bank;
    }
    config.session_id = non_empty_env("MNEMOPI_SESSION_ID");
    config
}

/// Serves the memory MCP server on stdin/stdout until EOF.
pub fn run_mcp(args: &[String]) -> Result<(), String> {
    let config = maho_memory::mcp::apply_args(mcp_config(), args)?;
    let stdin = std::io::stdin();
    config
        .serve(stdin.lock(), std::io::stdout().lock())
        .map_err(|error| format!("memory MCP server: {error}"))
}

fn people_enabled(cwd: &Path) -> bool {
    let view = maho_config::load_native(cwd, &maho_config::process_env()).view;
    maho_memory::extension::people_enabled(&view)
}

fn print(text: &str) {
    maho_core::output_guard::maho_write_stdout(&format!("{}\n", text.trim_end()));
}

/// `mhc memory ...`; returns the exit code.
pub fn run_memory_command(args: &[String]) -> Result<i32, String> {
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let memory = MemoryContext::new(&data_dir(), scoping(), &cwd);
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        None | Some("-h" | "--help" | "help") => {
            print(USAGE);
            Ok(0)
        }
        Some("status") => {
            print(&memory.status_text());
            Ok(0)
        }
        Some("search") => {
            if rest.is_empty() {
                return Err("usage: mhc memory search <query>".into());
            }
            let results = memory.recall(&rest.join(" "), 10)?;
            if results.is_empty() {
                print("No relevant memories found.");
            } else {
                print(&maho_memory::extension::recall_listing(&results));
            }
            Ok(0)
        }
        Some("remember") => {
            let global = rest.iter().any(|arg| arg == "--global");
            let text: Vec<&str> = rest
                .iter()
                .map(String::as_str)
                .filter(|arg| *arg != "--global")
                .collect();
            if text.is_empty() {
                return Err("usage: mhc memory remember [--global] <text>".into());
            }
            let id = memory.retain(&text.join(" "), Some("mhc memory remember"), global, "cli")?;
            print(&format!(
                "Stored memory {id} in bank {}.",
                if global {
                    DEFAULT_BANK
                } else {
                    memory.scope().bank.as_str()
                }
            ));
            Ok(0)
        }
        Some("import-legacy") => {
            let mut options = ImportOptions {
                root: legacy_root(),
                include_people: people_enabled(&cwd),
                dry_run: false,
            };
            let mut json = false;
            let mut iter = rest.iter();
            while let Some(arg) = iter.next() {
                match arg.as_str() {
                    "--from" => {
                        options.root = PathBuf::from(iter.next().ok_or("--from needs a directory")?)
                    }
                    "--dry-run" => options.dry_run = true,
                    "--include-people" => options.include_people = true,
                    "--json" => json = true,
                    other => return Err(format!("unknown import-legacy option: {other}")),
                }
            }
            let store = memory.open(DEFAULT_BANK)?;
            let report = import(&store, &options)?;
            if json {
                print(&serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?);
            } else {
                print(&report.text());
            }
            Ok(0)
        }
        Some("mcp") => run_mcp(rest).map(|()| 0),
        Some(other) => Err(format!(
            "unknown memory command '{other}' (see `mhc memory --help`)"
        )),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn mcp_entry_flags_include_the_legacy_name() {
        assert!(super::MCP_ENTRY_FLAGS.contains(&"--omo-memory-mcp"));
        assert!(super::MCP_ENTRY_FLAGS.contains(&"--memory-mcp"));
    }

    #[test]
    fn usage_names_every_command() {
        for command in ["status", "search", "remember", "import-legacy", "mcp"] {
            assert!(super::USAGE.contains(command));
        }
    }
}
