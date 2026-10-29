use std::{collections::BTreeMap, env, fs, path::PathBuf};

const MARKER_PREFIX: &str = "<!-- omo-ultrawork-astra:";
const FORBIDDEN: [&str; 8] = [
    "multi_agent", "spawn_agent", "update_plan", "wait_agent", "fork_context", "fork_turns", "codex", "wait_for",
];

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    let skill = manifest.join("src/directive-marked.txt");
    let variant = manifest.join("src/astra-variant.txt");
    println!("cargo:rerun-if-changed={}", skill.display());
    println!("cargo:rerun-if-changed={}", variant.display());
    let baseline_source = fs::read_to_string(&skill).expect("read ultrawork directive");
    let variant_source = fs::read_to_string(&variant).expect("read Astra sections");
    let baseline = transform(&baseline_source, None);
    let astra = transform(&baseline_source, Some(&variant_source));
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("build output directory"));
    fs::write(out.join("astra-directive.txt"), astra).expect("write generated Astra directive");
    let shipped = fs::read_to_string(manifest.join("src/directive.txt")).expect("read shipped baseline directive");
    assert_eq!(baseline, shipped, "baseline directive source drifted from generated marked source");
    assert!(!baseline.is_empty());
}

fn transform(source: &str, variant: Option<&str>) -> String {
    let normalized = normalize(source);
    let body = normalized.strip_prefix("---\n").and_then(|rest|rest.find("\n---\n").map(|end|&rest[end+5..])).unwrap_or(&normalized);
    let mut blocks = Vec::new();
    let mut open: Option<(String, usize, usize)> = None;
    let mut offset = 0;
    for line in body.split_inclusive('\n') {
        if let Some(rest) = line.strip_prefix(MARKER_PREFIX) {
            let (name, kind) = rest.strip_suffix(" -->\n").expect("marker line terminator").split_once(':').expect("marker kind");
            match kind {
                "start" => { assert!(open.is_none(), "nested Astra marker {name}"); assert!(!blocks.iter().any(|b: &Block| b.name == name), "duplicate Astra marker {name}"); open = Some((name.to_owned(), offset, offset + line.len())); }
                "end" => { let (open_name, marker_start, content_start) = open.take().expect("Astra end without start"); assert_eq!(open_name, name, "mismatched Astra marker"); blocks.push(Block { name: open_name, marker_start, content_start, content_end: offset, marker_end: offset + line.len() }); }
                _ => panic!("invalid Astra marker kind {kind}"),
            }
        }
        offset += line.len();
    }
    assert!(open.is_none(), "unclosed Astra marker");
    assert!(!blocks.is_empty(), "ultrawork directive has no Astra marker blocks");
    let sections = variant.map(parse_sections);
    if let Some(sections) = &sections {
        let missing = blocks.iter().filter(|b| !sections.contains_key(&b.name)).map(|b| b.name.as_str()).collect::<Vec<_>>();
        let extra = sections.keys().filter(|name| !blocks.iter().any(|b| &b.name == *name)).map(String::as_str).collect::<Vec<_>>();
        assert!(missing.is_empty() && extra.is_empty(), "Astra section parity mismatch (missing: {}; extra: {})", missing.join(", "), extra.join(", "));
    }
    let mut result = String::new(); let mut cursor = 0;
    for block in &blocks {
        result.push_str(&body[cursor..block.marker_start]);
        if let Some(sections) = &sections { result.push_str(sections.get(&block.name).expect("validated Astra section")); }
        else { result.push_str(&body[block.content_start..block.content_end]); }
        cursor = block.marker_end;
    }
    result.push_str(&body[cursor..]);
    while result.contains("\n\n\n") { result = result.replace("\n\n\n", "\n\n"); }
    let result = format!("{}\n", result.trim());
    let violations = result.split("\n\n").filter(|block| FORBIDDEN.iter().any(|token| block.to_ascii_lowercase().contains(token))).collect::<Vec<_>>();
    assert!(violations.is_empty(), "ultrawork directive contains forbidden tokens: {}", violations.join(" | "));
    result
}

struct Block { name: String, marker_start: usize, content_start: usize, content_end: usize, marker_end: usize }

fn normalize(value: &str) -> String { value.replace("\r\n", "\n").replace('\r', "\n") }

fn parse_sections(source: &str) -> BTreeMap<String, String> {
    let source = normalize(source);
    let headings = source.match_indices("## ").filter_map(|(start, _)| {
        let end = source[start..].find('\n').map(|n| start + n)?;
        let name = &source[start + 3..end];
        (!name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')).then_some((start, end + 1, name.to_owned()))
    }).collect::<Vec<_>>();
    assert!(!headings.is_empty() && headings[0].0 == 0, "Astra sections must start with a ## <name> heading");
    let mut sections = BTreeMap::new();
    for (i, (_, body_start, name)) in headings.iter().enumerate() {
        let body_end = headings.get(i + 1).map_or(source.len(), |h| h.0);
        let text = source[*body_start..body_end].trim();
        let section = if text.is_empty() { String::new() } else { format!("{text}\n") };
        assert!(sections.insert(name.clone(), section).is_none(), "duplicate Astra section {name}");
    }
    sections
}
