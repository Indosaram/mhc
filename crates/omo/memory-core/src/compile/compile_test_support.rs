//! Test helpers for memory compilation tests.

use std::collections::BTreeMap;
use std::path::PathBuf;

use tempfile::TempDir;

use crate::git::{GitMemoryRepo, GitMemoryRepoOptions, GitSeedFile, InitializeGitRepoOptions};

/// Parsed structural components of a compiled memory prompt block.
#[derive(Debug, PartialEq, Eq)]
pub struct CompiledBlockStructure {
    pub sections: Vec<String>,
    pub projection_paths: Vec<String>,
    pub memory_open_tags: Vec<String>,
    pub metadata: BTreeMap<String, String>,
}

/// Parse a compiled prompt block into its structural sections, tags, and metadata.
pub fn parse_compiled_block(block: &str) -> Result<CompiledBlockStructure, String> {
    let sections: Vec<String> = block
        .lines()
        .filter(|line| matches!(*line, "<self>" | "<memory>" | "<memory_metadata>"))
        .map(|line| line.trim_matches(['<', '>']).to_string())
        .collect();
    for section in &sections {
        let open_tag = format!("<{section}>");
        let close_tag = format!("</{section}>");
        let opens: Vec<usize> = line_offsets(block, &open_tag);
        let closes: Vec<usize> = line_offsets(block, &close_tag);
        if opens.len() != 1 || closes.len() != 1 || closes[0] <= opens[0] {
            return Err(format!("invalid compiled section: {section}"));
        }
    }

    let prefix = "<projection>$MEMORY_DIR/";
    let mut projection_paths = Vec::new();
    let mut rest = block;
    while let Some(at) = rest.find(prefix) {
        let after = &rest[at + prefix.len()..];
        let Some(close) = after.find("</projection>") else {
            break;
        };
        let path = &after[..close];
        if !path.is_empty() && !path.contains('<') {
            projection_paths.push(path.to_string());
        }
        rest = &after[close..];
    }

    let memory_open_tags = region(block, "memory")
        .map(|memory| {
            memory
                .lines()
                .filter_map(|line| {
                    let tag = line.trim_start().strip_prefix('<')?.strip_suffix('>')?;
                    let mut chars = tag.chars();
                    let first = chars.next()?;
                    let valid = first.is_ascii_lowercase()
                        && chars.all(|c| {
                            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'
                        });
                    valid.then(|| tag.to_string())
                })
                .collect()
        })
        .unwrap_or_default();

    let metadata_content = region(block, "memory_metadata")
        .ok_or_else(|| "missing compiled section: memory_metadata".to_string())?;
    let agent_id = metadata_content
        .lines()
        .find_map(|line| line.strip_prefix("- AGENT_ID: "))
        .ok_or_else(|| "missing compiled field: AGENT_ID".to_string())?;
    let metadata = BTreeMap::from([("agentId".to_string(), agent_id.to_string())]);

    Ok(CompiledBlockStructure {
        sections,
        projection_paths,
        memory_open_tags,
        metadata,
    })
}

fn line_offsets(block: &str, wanted: &str) -> Vec<usize> {
    let mut offset = 0;
    let mut found = Vec::new();
    for line in block.split('\n') {
        if line == wanted {
            found.push(offset);
        }
        offset += line.len() + 1;
    }
    found
}

/// Mirrors the TS `<tag>\n([\s\S]*?)\n</tag>` region match.
fn region(block: &str, tag: &str) -> Option<String> {
    let open_tag = format!("<{tag}>\n");
    let close_tag = format!("\n</{tag}>");
    let open_idx = block.find(&open_tag)?;
    let after_open = open_idx + open_tag.len();
    let close_idx = block[after_open..].find(&close_tag)?;
    Some(block[after_open..after_open + close_idx].to_string())
}

/// Helper for constructing markdown frontmatter and body.
pub fn memory(description: &str, body: &str) -> String {
    format!("---\ndescription: {description}\n---\n{body}")
}

/// Creates a temporary repository populated with the given seed files.
pub fn repo_with(files: Vec<(&str, &str)>) -> Result<(TempDir, GitMemoryRepo), String> {
    let temp_dir = TempDir::new().map_err(|e| e.to_string())?;
    let repo_dir: PathBuf = temp_dir.path().to_path_buf();
    let repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: repo_dir,
        agent_id: "fixture-agent".to_string(),
        exec: None,
        install_hooks: None,
    })
    .expect("open repo");

    let seed_files = files
        .into_iter()
        .map(|(path, content)| GitSeedFile {
            relative_path: path.to_string(),
            content: content.to_string(),
        })
        .collect();

    repo.init(InitializeGitRepoOptions {
        seed_files,
        author_name: Some("fixture-author".to_string()),
        install_hooks: None,
    })
    .map_err(|e| e.to_string())?;

    Ok((temp_dir, repo))
}
