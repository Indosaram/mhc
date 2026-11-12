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
  search [--at <iso>] <query>     Recall memories for this directory; --at asks what was
                                 true at that instant, against imported git history
  remember [--global] <text>     Store a memory in this project's bank (or the global bank)
  forget <memory-id>             Permanently delete one memory from this directory's banks
  update <memory-id> [--content <text>] [--importance <0..1>]
                                 Replace a memory's text or importance
  search-sessions [--include-hidden] <query>
                                 Search past session transcripts
  prune                          Drop rows past the retention windows (30d working, 90d episodic)
  import-legacy [--from <dir>] [--dry-run] [--include-people] [--history] [--json]
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

/// The first line worth showing for a hit.
///
/// A message can match on its reasoning or on a tool call's arguments while its visible text is empty, so falling
/// back in that order keeps a real hit from rendering as a blank line.
fn excerpt_of(document: &maho_memory::transcript_search::SearchDocument) -> String {
    let first_line = |value: &serde_json::Value| {
        value
            .as_str()
            .unwrap_or_default()
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
    };
    if let Some(content) = &document.content
        && !first_line(content).is_empty()
    {
        return first_line(content);
    }
    if let Some(reasoning) = &document.reasoning {
        let line = reasoning.lines().next().unwrap_or_default();
        if !line.is_empty() {
            return format!("(thinking) {line}");
        }
    }
    if let Some(calls) = &document.tool_calls
        && let Some(call) = calls.first()
    {
        let name = call.name.as_deref().unwrap_or("tool");
        return format!(
            "(call) {name} {}",
            call.arguments.as_deref().unwrap_or_default()
        )
        .trim_end()
        .to_string();
    }
    if let Some(output) = &document.tool_return {
        return format!("(output) {}", first_line(output));
    }
    String::new()
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
            let mut at: Option<&str> = None;
            let mut terms: Vec<&str> = Vec::new();
            let mut rest_iter = rest.iter().map(String::as_str);
            while let Some(arg) = rest_iter.next() {
                if let Some(value) = arg.strip_prefix("--at=") {
                    at = Some(value);
                } else if arg == "--at" {
                    at = Some(rest_iter.next().ok_or_else(|| {
                        "--at needs an ISO instant, e.g. --at 2026-03-01".to_string()
                    })?);
                } else {
                    terms.push(arg);
                }
            }
            if terms.is_empty() {
                return Err("usage: mhc memory search [--at <iso>] <query>".into());
            }
            let (results, resolved) = memory.recall_at(&terms.join(" "), 10, at)?;
            if results.is_empty() {
                print("No relevant memories found.");
            } else {
                match resolved {
                    Some(instant) => print(&maho_memory::extension::recall_listing_at(
                        &results, &instant,
                    )),
                    None => print(&maho_memory::extension::recall_listing(&results)),
                }
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
        Some("forget") => {
            let id = match rest.first() {
                Some(id) if !id.starts_with("--") => id.as_str(),
                _ => return Err("usage: mhc memory forget <memory-id>".into()),
            };
            match memory.forget(id)? {
                Some(bank) => print(&format!("Deleted memory {id} from bank {bank}.")),
                None => print(&format!("No memory {id} in this directory's banks.")),
            }
            Ok(0)
        }
        Some("update") => {
            let mut id: Option<&str> = None;
            let mut content: Option<&str> = None;
            let mut importance: Option<f64> = None;
            let mut iter = rest.iter();
            while let Some(arg) = iter.next() {
                match arg.as_str() {
                    "--content" => content = Some(iter.next().ok_or("--content needs text")?),
                    "--importance" => {
                        let raw = iter.next().ok_or("--importance needs a number (0..1)")?;
                        let value: f64 = raw.parse().map_err(|_| format!("not a number: {raw}"))?;
                        if !(0.0..=1.0).contains(&value) {
                            return Err(format!("importance must be within 0..=1, got {value}"));
                        }
                        importance = Some(value);
                    }
                    other if id.is_none() && !other.starts_with("--") => id = Some(other),
                    other => return Err(format!("unknown update option: {other}")),
                }
            }
            let Some(id) = id else {
                return Err(
                    "usage: mhc memory update <memory-id> [--content <text>] [--importance <0..1>]"
                        .into(),
                );
            };
            if content.is_none() && importance.is_none() {
                return Err("nothing to change: pass --content and/or --importance".into());
            }
            match memory.update(id, content, importance)? {
                Some(bank) => print(&format!("Updated memory {id} in bank {bank}.")),
                None => print(&format!("No memory {id} in this directory's banks.")),
            }
            Ok(0)
        }
        Some("import-legacy") => {
            let mut options = ImportOptions {
                root: legacy_root(),
                include_people: people_enabled(&cwd),
                dry_run: false,
                history: false,
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
                    "--history" => options.history = true,
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
        Some("search-sessions") => {
            let include_hidden = rest.iter().any(|arg| arg == "--include-hidden");
            let query: Vec<&str> = rest
                .iter()
                .map(String::as_str)
                .filter(|arg| *arg != "--include-hidden")
                .collect();
            if query.is_empty() {
                return Err("usage: mhc memory search-sessions [--include-hidden] <query>".into());
            }
            let provider = maho_memory::session_provider::SessionProvider::new(
                maho_memory::session_provider::SessionProviderOptions {
                    sessions_dir: PathBuf::from(maho_core::config::get_sessions_dir()),
                    excluded_dirs: None,
                    hidden_marker_file: Some(".hidden".into()),
                    is_hidden: None,
                },
            );
            let results = maho_memory::transcript_search::search_transcripts(
                &provider,
                &query.join(" "),
                &maho_memory::transcript_search::SearchOptions {
                    limit: Some(10),
                    include_hidden: include_hidden.then_some(true),
                    ..Default::default()
                },
            );
            if results.is_empty() {
                print("No matching sessions found.");
            } else {
                print(&format!("Found {} matching messages:", results.len()));
                for result in &results {
                    print(&format!(
                        "- [{}] {} ({})",
                        result.conversation_id,
                        excerpt_of(&result.document),
                        result.created_at
                    ));
                }
            }
            Ok(0)
        }
        Some("prune") => {
            let reports = memory.prune(&maho_memory::retention::RetentionPolicy::default())?;
            if reports.is_empty() {
                print("Nothing to prune.");
            } else {
                for (bank, report) in &reports {
                    print(&format!(
                        "{bank}: {} working, {} episodic dropped",
                        report.working, report.episodic
                    ));
                }
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
        for command in [
            "status",
            "search",
            "remember",
            "prune",
            "import-legacy",
            "mcp",
        ] {
            assert!(super::USAGE.contains(command));
        }
    }
}
