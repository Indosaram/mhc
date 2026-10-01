use maho_ai::legacy_provider_ids::normalize_provider_id;
use serde_json::{Map, Value};
use std::{fs, io::Write, path::{Path, PathBuf}};
use crate::settings_manager::parse_settings_json;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelsJsonMigration {
    Unchanged,
    Migrated { renamed: Vec<String>, backup_path: PathBuf },
    Failed { renamed: Vec<String>, reason: String },
}

#[derive(Debug)]
struct Node { start: usize, end: usize, members: Vec<(String, usize, usize, Node)>, elements: Vec<Node> }

fn trivia(text: &[u8], mut i: usize) -> Result<usize, String> {
    while i < text.len() {
        if text[i].is_ascii_whitespace() { i += 1; }
        else if text[i..].starts_with(&[0xef, 0xbb, 0xbf]) { i += 3; }
        else if text[i..].starts_with(b"//") { i += text[i..].iter().position(|b| *b == b'\n').unwrap_or(text.len() - i); }
        else if text[i..].starts_with(b"/*") {
            i += text[i + 2..].windows(2).position(|w| w == b"*/").ok_or("unterminated comment")? + 4;
        } else { break; }
    }
    Ok(i)
}

fn string_end(text: &[u8], start: usize) -> Result<usize, String> {
    let mut i = start + 1;
    while i < text.len() {
        match text[i] { b'\\' => i += 2, b'"' => return Ok(i + 1), _ => i += 1 }
    }
    Err("unterminated string".into())
}

fn scan(text: &str, from: usize) -> Result<Node, String> {
    let bytes = text.as_bytes();
    let start = trivia(bytes, from)?;
    let mut node = Node { start, end: start, members: Vec::new(), elements: Vec::new() };
    match bytes.get(start) {
        Some(b'{') | Some(b'[') => {
            let object = bytes[start] == b'{';
            let mut i = start + 1;
            loop {
                i = trivia(bytes, i)?;
                match bytes.get(i) {
                    Some(b'}') if object => { node.end = i + 1; return Ok(node); }
                    Some(b']') if !object => { node.end = i + 1; return Ok(node); }
                    Some(b',') => { i += 1; continue; }
                    None => return Err("unterminated container".into()),
                    _ => {}
                }
                if object {
                    if bytes[i] != b'"' { return Err(format!("expected a key at offset {i}")); }
                    let end = string_end(bytes, i)?;
                    let key = serde_json::from_str(&text[i..end]).map_err(|e| e.to_string())?;
                    let colon = trivia(bytes, end)?;
                    if bytes.get(colon) != Some(&b':') { return Err(format!("expected ':' at offset {colon}")); }
                    let value = scan(text, colon + 1)?;
                    let next = value.end;
                    node.members.push((key, i, end, value));
                    i = next;
                } else {
                    let value = scan(text, i)?;
                    i = value.end;
                    node.elements.push(value);
                }
            }
        }
        Some(b'"') => node.end = string_end(bytes, start)?,
        Some(_) => {
            node.end = start;
            while bytes.get(node.end).is_some_and(|b| !b.is_ascii_whitespace() && !b",]}/".contains(b)) { node.end += 1; }
            if node.end == start { return Err(format!("unexpected character at offset {start}")); }
        }
        None => return Err("unexpected end".into()),
    }
    Ok(node)
}

fn rewrite(content: &str, expected: &Map<String, Value>) -> Result<String, String> {
    let root = scan(content, 0)?;
    let bytes = content.as_bytes();
    let mut edits = Vec::new();
    for (key, _, _, node) in &root.members {
        if key == "providers" {
            for (id, start, end, value) in &node.members {
                let canonical = normalize_provider_id(id);
                if canonical == *id { continue; }
                if node.members.iter().any(|(name, _, _, _)| *name == canonical) {
                    let after = trivia(bytes, value.end)?;
                    let (start, end) = if bytes.get(after) == Some(&b',') {
                        let mut begin = *start;
                        while begin > 0 && matches!(bytes[begin - 1], b' ' | b'\t') { begin -= 1; }
                        let owns_line = begin == 0 || bytes[begin - 1] == b'\n';
                        let mut finish = after + 1;
                        while bytes.get(finish).is_some_and(|b| matches!(b, b' ' | b'\t')) { finish += 1; }
                        if owns_line && bytes.get(finish) == Some(&b'\r') { finish += 1; }
                        if owns_line && bytes.get(finish) == Some(&b'\n') { (begin, finish + 1) }
                        else { (if owns_line { begin } else { *start }, after + 1) }
                    } else {
                        let mut comma = *start;
                        while comma > 0 && bytes[comma - 1].is_ascii_whitespace() { comma -= 1; }
                        (if comma > 0 && bytes[comma - 1] == b',' { comma - 1 } else { *start }, value.end)
                    };
                    edits.push((start, end, String::new()));
                } else { edits.push((*start, *end, serde_json::to_string(&canonical).map_err(|e| e.to_string())?)); }
            }
        }
        if key == "disabledProviders" {
            for element in &node.elements {
                if let Ok(id) = serde_json::from_str::<String>(&content[element.start..element.end]) {
                    let canonical = normalize_provider_id(&id);
                    if canonical != id { edits.push((element.start, element.end, serde_json::to_string(&canonical).map_err(|e| e.to_string())?)); }
                }
            }
        }
    }
    edits.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
    let mut result = content.to_owned();
    for (start, end, replacement) in edits { result.replace_range(start..end, &replacement); }
    if parse_settings_json(&result).as_ref() == Ok(expected) { Ok(result) }
    else { serde_json::to_string_pretty(expected).map(|s| s + "\n").map_err(|e| e.to_string()) }
}

pub fn migrate_models_json_provider_ids(path: &Path, content: &str) -> ModelsJsonMigration {
    let Ok(mut next) = parse_settings_json(content) else { return ModelsJsonMigration::Unchanged; };
    let mut renamed = Vec::new();
    if let Some(Value::Object(providers)) = next.get_mut("providers") {
        let mut canonical_providers = Map::new();
        for (id, provider) in providers.iter() {
            let canonical = normalize_provider_id(id);
            if canonical != *id {
                renamed.push(format!("{id} -> {canonical}"));
                if providers.contains_key(&canonical) { continue; }
            }
            canonical_providers.insert(canonical, provider.clone());
        }
        *providers = canonical_providers;
    }
    if let Some(Value::Array(disabled)) = next.get_mut("disabledProviders") {
        for value in disabled {
            if let Some(id) = value.as_str() {
                let canonical = normalize_provider_id(id);
                if canonical != id {
                    let notice = format!("{id} -> {canonical}");
                    if !renamed.contains(&notice) { renamed.push(notice); }
                    *value = Value::String(canonical);
                }
            }
        }
    }
    if renamed.is_empty() { return ModelsJsonMigration::Unchanged; }
    let stamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true).replace([':', '.'], "-");
    let base = format!("{}.backup-{stamp}", path.display());
    let mut backup = PathBuf::from(&base);
    let mut attempt = 1;
    while backup.exists() { backup = PathBuf::from(format!("{base}-{attempt}")); attempt += 1; }
    let temporary = PathBuf::from(format!("{}.{}.tmp", path.display(), std::process::id()));
    let mut created = Vec::new();
    let result = (|| -> Result<(), String> {
        let permissions = fs::metadata(path).map_err(|e| e.to_string())?.permissions();
        let replacement = rewrite(content, &next).or_else(|_| serde_json::to_string_pretty(&next).map(|s| s + "\n").map_err(|e| e.to_string()))?;
        for (target, data) in [(&backup, content), (&temporary, replacement.as_str())] {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            { use std::os::unix::fs::{OpenOptionsExt, PermissionsExt}; options.mode(permissions.mode() & 0o777); }
            let mut file = options.open(target).map_err(|e| e.to_string())?;
            created.push(target.clone());
            file.write_all(data.as_bytes()).map_err(|e| e.to_string())?;
        }
        fs::set_permissions(&temporary, permissions).map_err(|e| e.to_string())?;
        if fs::read_to_string(path).map_err(|e| e.to_string())? != content { return Err("the file changed while it was being migrated".into()); }
        fs::rename(&temporary, path).map_err(|e| e.to_string())
    })();
    match result {
        Ok(()) => ModelsJsonMigration::Migrated { renamed, backup_path: backup },
        Err(reason) => {
            for target in created { if let Err(error) = fs::remove_file(&target) && error.kind() != std::io::ErrorKind::NotFound { eprintln!("Unable to remove migration temporary file: {error}"); } }
            ModelsJsonMigration::Failed { renamed, reason }
        }
    }
}
