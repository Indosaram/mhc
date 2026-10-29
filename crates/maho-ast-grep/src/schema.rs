//! Published tool definitions (spec §3). No composition keywords anywhere.

use crate::params::{
    DEFAULT_MAX_MATCHES, LANGUAGES, MAX_MATCHES, MAX_TIMEOUT_MS, MIN_TIMEOUT_MS, STRICTNESS,
};
use serde_json::{Map, Value, json};

fn common_properties() -> Map<String, Value> {
    let mut props = Map::new();
    props.insert(
        "paths".into(),
        json!({
            "type": "array", "minItems": 1, "maxItems": 64,
            "items": { "type": "string", "minLength": 1, "maxLength": 4096 },
            "description": "Files or directories to search (1-64 entries). Required; there is no default."
        }),
    );
    props.insert(
        "workdir".into(),
        json!({
            "type": "string", "minLength": 1, "maxLength": 4096,
            "description": "Directory the program runs in. Defaults to the project working directory."
        }),
    );
    props.insert(
        "globs".into(),
        json!({
            "type": "array", "maxItems": 32,
            "items": { "type": "string", "minLength": 1, "maxLength": 1024 },
            "description": "Include/exclude globs; prefix a glob with ! to exclude."
        }),
    );
    props.insert(
        "maxMatches".into(),
        json!({
            "type": "integer", "minimum": 1, "maximum": MAX_MATCHES, "default": DEFAULT_MAX_MATCHES,
            "description": "Stop after this many matches."
        }),
    );
    props.insert(
        "timeoutMs".into(),
        json!({
            "type": "integer", "minimum": MIN_TIMEOUT_MS, "maximum": MAX_TIMEOUT_MS,
            "default": MAX_TIMEOUT_MS,
            "description": "Time budget in milliseconds for the whole call."
        }),
    );
    props.insert(
        "includeHidden".into(),
        json!({ "type": "boolean", "description": "Also search hidden files." }),
    );
    props.insert(
        "followSymlinks".into(),
        json!({ "type": "boolean", "description": "Follow symbolic links." }),
    );
    props
}

fn pattern_properties(props: &mut Map<String, Value>) {
    props.insert(
        "pattern".into(),
        json!({
            "type": "string", "minLength": 1,
            "description": "ast-grep pattern: code with $NAME / $$$NAME metavariables, not a regular expression. At most 16384 bytes of UTF-8."
        }),
    );
    props.insert(
        "language".into(),
        json!({ "type": "string", "enum": LANGUAGES, "description": "Language the pattern is parsed in." }),
    );
    props.insert(
        "selector".into(),
        json!({ "type": "string", "minLength": 1, "maxLength": 128, "description": "Sub-node kind to select from the pattern." }),
    );
    props.insert(
        "strictness".into(),
        json!({ "type": "string", "enum": STRICTNESS, "default": "smart", "description": "Match strictness." }),
    );
    props.insert(
        "force".into(),
        json!({ "type": "boolean", "description": "Run even when the pattern preflight rejects it with a hint." }),
    );
}

fn object(props: Map<String, Value>, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": props,
        "required": required,
        "additionalProperties": false
    })
}

/// The `tools/list` entries.
pub fn tool_definitions() -> Vec<Value> {
    let mut search = common_properties();
    pattern_properties(&mut search);

    let mut rewrite = common_properties();
    pattern_properties(&mut rewrite);
    rewrite.insert(
        "rewrite".into(),
        json!({
            "type": "string",
            "description": "Replacement code using the pattern's metavariables; the empty string removes the match. At most 65536 bytes of UTF-8."
        }),
    );
    rewrite.insert(
        "apply".into(),
        json!({ "type": "boolean", "default": false, "description": "Write the changes. Default false is a dry run." }),
    );

    let mut scan = common_properties();
    scan.insert(
        "ruleFile".into(),
        json!({ "type": "string", "minLength": 1, "maxLength": 4096, "description": "Path of a YAML rule file. Give exactly one of ruleFile and inlineRules." }),
    );
    scan.insert(
        "inlineRules".into(),
        json!({ "type": "string", "minLength": 1, "description": "YAML rule text, at most 65536 bytes of UTF-8. Give exactly one of ruleFile and inlineRules." }),
    );
    scan.insert(
        "includeMetadata".into(),
        json!({ "type": "boolean", "description": "Include rule metadata in each match." }),
    );
    scan.insert(
        "apply".into(),
        json!({ "type": "boolean", "default": false, "description": "Apply rule fixes. Default false is a dry run." }),
    );

    vec![
        json!({
            "name": "search",
            "description": "Search source code by syntax tree with an ast-grep pattern. Returns matches with positions and metavariable captures.",
            "inputSchema": object(search, &["paths", "pattern", "language"]),
        }),
        json!({
            "name": "rewrite",
            "description": "Rewrite code matched by an ast-grep pattern. Dry run unless apply is true; a truncated preview is never applied.",
            "inputSchema": object(rewrite, &["paths", "pattern", "language", "rewrite"]),
        }),
        json!({
            "name": "scan",
            "description": "Scan code with ast-grep YAML rules (a rule file or inline rules). Dry run unless apply is true.",
            "inputSchema": object(scan, &["paths"]),
        }),
    ]
}
