use crate::{
    arity,
    bash_command_segments::{BashCommandAnalysis, analyze_bash_command, best_effort_command_lines},
    external_dir::{
        expand_home, extract_command_paths, extract_external_paths, is_credential_path,
        lexical_resolve, resolve_external_path, to_parent_directory_pattern,
        to_parent_directory_pattern_from,
    },
};
pub use maho_ext_api::ToolDefinition;
use serde_json::Value;
use std::{collections::BTreeMap, path::Path, sync::Arc};

/// A permission request without ID/session metadata (TS `PermissionRequest`).
///
/// DIVERGENCE(senpi v2026.10.27-2): upstream extends the shared request type with
/// `originalPaths`/`ruleAliases`/`autoApproveAsk`; here the fields live on this crate-local type
/// because the tool-facing `maho_tools::ToolPermissionRequest` (custom parsers) has only
/// permission/patterns/always. Custom parser output converts through `From`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PermissionRequest {
    pub permission: String,
    pub patterns: Vec<String>,
    pub always: Vec<String>,
    /// Raw inputs of external targets, for display and deny checks only (senpi#4311).
    pub original_paths: Option<Vec<String>>,
    /// Absolute external lexical spellings restrict the resolved decision; other aliases share rule order.
    pub rule_aliases: Option<Vec<String>>,
    /// Only a deny decides this request; an ask is approved without prompting.
    pub auto_approve_ask: bool,
    /// A wider "Allow always" the prompt may offer (senpi#3403, `always-scope.ts`).
    pub wider_always: Option<crate::always_scope::WiderAlwaysScope>,
}
impl From<maho_ext_api::ToolPermissionRequest> for PermissionRequest {
    fn from(request: maho_ext_api::ToolPermissionRequest) -> Self {
        Self {
            permission: request.permission,
            patterns: request.patterns,
            always: request.always,
            ..Self::default()
        }
    }
}

pub type ToolPermissionParser =
    Arc<dyn Fn(&Value, &Path, &Path) -> Vec<PermissionRequest> + Send + Sync>;
pub type ToolDefinitionResolver = Arc<dyn Fn(&str) -> Option<ToolDefinition> + Send + Sync>;

#[derive(Default, Clone)]
pub struct ParserRegistry {
    parsers: BTreeMap<String, ToolPermissionParser>,
    definition_resolver: Option<ToolDefinitionResolver>,
}
fn request(permission: &str, patterns: Vec<String>, always: Vec<String>) -> PermissionRequest {
    PermissionRequest {
        permission: permission.into(),
        patterns,
        always,
        ..PermissionRequest::default()
    }
}
fn fallback(permission: &str) -> Vec<PermissionRequest> {
    vec![request(permission, vec!["*".into()], vec!["*".into()])]
}
fn string<'a>(input: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| input.get(key).and_then(Value::as_str))
}
fn uniq(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for value in values {
        if !out.contains(&value) {
            out.push(value);
        }
    }
    out
}

/// The paths a tool may actually open for one raw argument. The `external_directory` check must
/// classify exactly these, never the raw text, or a spelling the tool normalizes away (`@`,
/// `file://`, Unicode spaces, Windows shell drives) slips past the check (senpi#4311).
#[derive(Clone, Copy)]
enum TargetResolver {
    /// The raw argument as written: tools that resolve with a plain path join (bash, monitor).
    Raw,
    /// write/edit/ls/find/glob open `resolve_to_cwd(raw, cwd)`.
    Tool,
    /// apply_patch writes `resolve_patch_path(cwd, path)` for every patch-body path.
    Patch,
    /// No known tool implementation: the union of the raw and resolved readings.
    UnknownTool,
    /// grep: an existing literal name wins; only a missing name with a file prefix is a selector.
    Grep,
}
/// `resolveToCwd(raw, cwd)`: the tool's path expansion, then a lexical resolve that keeps the root
/// as written (a drive-less `/etc` stays `/etc` on Windows, a `\\?\C:\` root keeps its prefix).
fn to_cwd(raw: &str, cwd: &Path) -> String {
    lexical_resolve(cwd, &maho_tools::path_utils::expand_path(raw))
        .to_string_lossy()
        .into_owned()
}
impl TargetResolver {
    fn resolve(self, raw: &str, cwd: &Path) -> Vec<String> {
        match self {
            Self::Raw => vec![raw.to_owned()],
            Self::Tool => vec![to_cwd(raw, cwd)],
            Self::Patch => vec![
                maho_ext_gpt_apply_patch::workspace::resolve_patch_path(cwd, Path::new(raw))
                    .to_string_lossy()
                    .into_owned(),
            ],
            Self::UnknownTool => vec![raw.to_owned(), to_cwd(raw, cwd)],
            Self::Grep => vec![grep_target(&to_cwd(raw, cwd))],
        }
    }
}
/// `/^(.*):L(\d+)-L?(\d+)$/` -> the file part.
fn grep_selector_file(resolved: &str) -> Option<&str> {
    let (file, range) = resolved.rsplit_once(":L")?;
    let (start, end) = range.split_once('-')?;
    let end = end.strip_prefix('L').unwrap_or(end);
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    (digits(start) && digits(end)).then_some(file)
}
/// Match the grep tool: only a missing name with a `file:Lx-Ly` suffix whose file exists is a
/// selector. DIVERGENCE: upstream throws on a non-ENOENT stat error (blocking the call); a parser
/// here cannot fail, so such a path is classified as written (the literal the tool would open).
fn grep_target(resolved: &str) -> String {
    let Some(file) = grep_selector_file(resolved) else {
        return resolved.to_owned();
    };
    if std::fs::metadata(resolved).is_ok() {
        return resolved.to_owned();
    }
    if std::fs::metadata(file).is_ok_and(|meta| meta.is_file()) {
        file.to_owned()
    } else {
        resolved.to_owned()
    }
}

/// External-directory classification plus the credential check over the same paths.
fn with_external_directory_requests(
    requests: Vec<PermissionRequest>,
    paths: &[String],
    location: (&Path, &Path),
    directory: bool,
    resolver: TargetResolver,
    agent_dir: &Path,
) -> Vec<PermissionRequest> {
    credential(
        external_directory_requests(requests, paths, location, directory, resolver),
        paths,
        location,
        agent_dir,
    )
}
fn external_directory_requests(
    mut requests: Vec<PermissionRequest>,
    paths: &[String],
    location: (&Path, &Path),
    directory: bool,
    resolver: TargetResolver,
) -> Vec<PermissionRequest> {
    let (cwd, home) = location;
    // Classify the paths the tool opens (senpi#4316), each through the canonical spelling walk (senpi#4351).
    let opened: Vec<(&String, String, String)> = paths
        .iter()
        .flat_map(|input| {
            resolver
                .resolve(input, cwd)
                .into_iter()
                .filter_map(move |opened| {
                    let target = resolve_external_path(&opened, cwd, home)?;
                    Some((input, opened, target))
                })
        })
        .collect();
    // One pattern per canonical target, but every opened spelling keeps its lexical alias, so a rule
    // on a second spelling of the same target still decides the call (senpi#4316).
    let mut seen = Vec::new();
    let external: Vec<&(&String, String, String)> = opened
        .iter()
        .filter(|(_, _, target)| {
            if seen.contains(target) {
                false
            } else {
                seen.push(target.clone());
                true
            }
        })
        .collect();
    if !external.is_empty() {
        let aliases = uniq(opened.iter().filter_map(|(_, opened, target)| {
            let lexical = lexical_resolve(cwd, &expand_home(opened, home))
                .to_string_lossy()
                .into_owned();
            (lexical != *target).then_some(lexical)
        }));
        let original = external
            .iter()
            .any(|(input, _, target)| *input != target)
            .then(|| {
                external
                    .iter()
                    .map(|(input, _, _)| (*input).clone())
                    .collect()
            });
        requests.push(PermissionRequest {
            permission: "external_directory".into(),
            patterns: external
                .iter()
                .map(|(_, _, target)| target.clone())
                .collect(),
            always: external
                .iter()
                .map(|(input, _, target)| {
                    to_parent_directory_pattern_from(target, directory, input)
                })
                .collect(),
            original_paths: original,
            rule_aliases: (!aliases.is_empty()).then_some(aliases),
            auto_approve_ask: false,
            wider_always: None,
        });
    }
    requests
}
/// Emits `credential_file` for any path that is one of mhc's credential files, whether or not it is external.
fn credential(
    mut requests: Vec<PermissionRequest>,
    paths: &[String],
    location: (&Path, &Path),
    agent_dir: &Path,
) -> Vec<PermissionRequest> {
    let paths: Vec<String> = paths
        .iter()
        .filter(|path| is_credential_path(path, location.0, location.1, agent_dir))
        .cloned()
        .collect();
    if !paths.is_empty() {
        requests.push(request("credential_file", paths.clone(), paths));
    }
    requests
}
fn fallback_for_tool(name: &str, input: &Value) -> Vec<PermissionRequest> {
    let Some((key, arg)) = [
        "path",
        "file_path",
        "command",
        "url",
        "input",
        "pattern",
        "filePath",
    ]
    .iter()
    .find_map(|key| {
        input
            .get(key)
            .and_then(Value::as_str)
            .map(|arg| (*key, arg))
    }) else {
        return fallback(name);
    };
    // "Allow always" keeps the exact arg; a wider scope is a separate, labelled choice (senpi#3403).
    let mut request = request(name, vec![arg.into()], vec![arg.into()]);
    request.wider_always = crate::always_scope::wider_always_scope(name, key, arg);
    vec![request]
}

/// The targets an "Allow always" answer may store. Stored rules are matched as globs, so a target
/// holding its own `*` or `?` would be remembered as a wildcard that matches other commands; such a
/// target is allowed once and never stored. Request patterns stay raw.
fn rememberable(targets: &[String]) -> Vec<String> {
    targets
        .iter()
        .filter(|target| !target.contains(['*', '?']))
        .cloned()
        .collect()
}

/// Marks a bash request target that no command-specific rule can match: only a `*` rule decides it.
pub const UNPARSED_BASH_PREFIX: &str = "(unparsed) ";

fn arity_prefix(words: &[String]) -> String {
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    arity::prefix(&words).join(" ")
}

/// The `bash` requests for one command (senpi#4309). Every simple command it runs is decided, so
/// the call is as strict as its strictest command: one pattern per command (its arity prefix), all
/// of which must be allowed, and "Allow always" stores exactly those prefixes. A command the lexer
/// cannot parse is one target only a `*` rule can decide; its plain words are still checked
/// against deny rules first.
pub fn bash_command_requests(command: &str) -> Vec<PermissionRequest> {
    match analyze_bash_command(command) {
        BashCommandAnalysis::Unparsed => {
            let target = format!("{UNPARSED_BASH_PREFIX}{command}");
            let candidates = uniq(
                best_effort_command_lines(command)
                    .iter()
                    .map(|words| arity_prefix(words))
                    .filter(|prefix| !prefix.is_empty()),
            );
            let mut requests = Vec::new();
            // The deny-only check runs first, so a denied call is refused before anyone is asked.
            if !candidates.is_empty() {
                requests.push(PermissionRequest {
                    permission: "bash".into(),
                    patterns: candidates,
                    always: Vec::new(),
                    auto_approve_ask: true,
                    ..PermissionRequest::default()
                });
            }
            let always = rememberable(std::slice::from_ref(&target));
            requests.push(request("bash", vec![target], always));
            requests
        }
        BashCommandAnalysis::Parsed { segments } => {
            let prefixes = uniq(
                segments
                    .iter()
                    .map(|words| arity_prefix(words))
                    .filter(|prefix| !prefix.is_empty()),
            );
            if prefixes.is_empty() {
                return vec![request(
                    "bash",
                    vec![command.into()],
                    rememberable(&[command.into()]),
                )];
            }
            let always = rememberable(&prefixes)
                .into_iter()
                .flat_map(|prefix| [prefix.clone(), format!("{prefix} *")])
                .collect();
            vec![request("bash", prefixes, always)]
        }
    }
}

impl ParserRegistry {
    pub fn register(&mut self, name: &str, parser: ToolPermissionParser) {
        self.parsers.insert(name.into(), parser);
    }
    pub fn set_definition_resolver(&mut self, resolver: ToolDefinitionResolver) {
        self.definition_resolver = Some(resolver);
    }
    pub fn parse(
        &self,
        name: &str,
        input: &Value,
        location: (&Path, &Path),
    ) -> Vec<PermissionRequest> {
        if let Some(parser) = self.parsers.get(name) {
            return parser(input, location.0, location.1);
        }
        if let Some(resolver) = &self.definition_resolver
            && let Some(definition) = resolver(name)
            && let Some(custom) = &definition.permission_parser
        {
            return custom(input, location.0)
                .into_iter()
                .map(PermissionRequest::from)
                .collect();
        }
        fallback_for_tool(name, input)
    }
}
/// Registry whose credential check uses the process default agent dir; sessions use [`create_builtin_parser_registry_in`].
pub fn create_builtin_parser_registry() -> ParserRegistry {
    create_builtin_parser_registry_in(Path::new(&maho_core::config::get_agent_dir()))
}
/// Registry whose `credential_file` check protects `agent_dir` (the session's agent dir).
pub fn create_builtin_parser_registry_in(agent_dir: &Path) -> ParserRegistry {
    let mut registry = ParserRegistry::default();
    for (name, key) in [("bash", "command"), ("bash_input", "input")] {
        let agent = agent_dir.to_path_buf();
        registry.register(
            name,
            Arc::new(move |input, cwd, home| {
                let Some(command) = string(input, &[key]).filter(|command| !command.is_empty())
                else {
                    return fallback("bash");
                };
                let requests = bash_command_requests(command);
                let paths = extract_external_paths(command, cwd, home);
                let requests = external_directory_requests(
                    requests,
                    &paths,
                    (cwd, home),
                    false,
                    TargetResolver::Raw,
                );
                // Credential files are checked over every path-like token, external or not.
                credential(
                    requests,
                    &extract_command_paths(command),
                    (cwd, home),
                    &agent,
                )
            }),
        );
    }
    let bash = Arc::clone(
        registry
            .parsers
            .get("bash")
            .expect("bash parser registered"),
    );
    let agent = agent_dir.to_path_buf();
    registry.register(
        "monitor",
        Arc::new(move |input, cwd, home| {
            if let Some(path) = string(input, &["path"]).filter(|path| !path.is_empty()) {
                let paths = vec![path.into()];
                return with_external_directory_requests(
                    vec![request("read", paths.clone(), paths.clone())],
                    &paths,
                    (cwd, home),
                    false,
                    TargetResolver::Raw,
                    &agent,
                );
            }
            bash(input, cwd, home)
        }),
    );
    for (name, permission) in [
        ("edit", "edit"),
        ("write", "edit"),
        ("apply_patch", "edit"),
        ("multiedit", "edit"),
        ("read_permission", "read"),
    ] {
        let agent = agent_dir.to_path_buf();
        registry.register(
            name,
            Arc::new(move |input, cwd, home| {
                let (paths, resolver) = if let Some(path) =
                    string(input, &["path", "file_path"]).filter(|path| !path.is_empty())
                {
                    (
                        vec![path.into()],
                        if name == "edit" || name == "write" {
                            TargetResolver::Tool
                        } else {
                            TargetResolver::UnknownTool
                        },
                    )
                } else if permission == "edit" {
                    (
                        maho_ext_gpt_apply_patch::text::extract_patched_paths(
                            string(input, &["input", "patchText"]).unwrap_or_default(),
                        ),
                        TargetResolver::Patch,
                    )
                } else {
                    (Vec::new(), TargetResolver::Raw)
                };
                if paths.is_empty() {
                    return fallback(permission);
                }
                let requests = paths
                    .iter()
                    .map(|path| {
                        request(
                            permission,
                            vec![path.clone()],
                            vec![if permission == "read" {
                                to_parent_directory_pattern(path, false)
                            } else {
                                path.clone()
                            }],
                        )
                    })
                    .collect();
                with_external_directory_requests(
                    requests,
                    &paths,
                    (cwd, home),
                    false,
                    resolver,
                    &agent,
                )
            }),
        );
    }
    let agent = agent_dir.to_path_buf();
    registry.register(
        "read",
        Arc::new(move |input, cwd, home| parse_read_permission(input, (cwd, home), &agent)),
    );
    let agent = agent_dir.to_path_buf();
    registry.register(
        "grep",
        Arc::new(move |input, cwd, home| {
            // grep's `path` is a string or an array of strings, and the tool searches every element.
            let search_paths: Vec<String> = match input.get("path") {
                Some(Value::String(path)) => vec![path.clone()],
                Some(Value::Array(paths)) => paths
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                _ => Vec::new(),
            };
            let patterns = if !search_paths.is_empty() {
                search_paths.clone()
            } else {
                string(input, &["pattern"])
                    .filter(|pattern| !pattern.is_empty())
                    .map(|pattern| vec![pattern.to_owned()])
                    .unwrap_or_default()
            };
            if patterns.is_empty() {
                return fallback("grep");
            }
            let requests = vec![request("grep", patterns, vec!["*".into()])];
            if search_paths.is_empty() {
                requests
            } else {
                with_external_directory_requests(
                    requests,
                    &search_paths,
                    (cwd, home),
                    true,
                    TargetResolver::Grep,
                    &agent,
                )
            }
        }),
    );
    for name in ["find", "glob", "ls"] {
        let agent = agent_dir.to_path_buf();
        registry.register(
            name,
            Arc::new(move |input, cwd, home| {
                let path = string(input, &["path"]).unwrap_or(".");
                let paths = vec![path.into()];
                with_external_directory_requests(
                    vec![request("list", paths.clone(), paths.clone())],
                    &paths,
                    (cwd, home),
                    true,
                    TargetResolver::Tool,
                    &agent,
                )
            }),
        );
    }
    registry
}

/// Synchronous port of `resolveReadPath` (path-utils.ts), the same probing the read tool does in
/// `maho_tools::path_utils::resolve_read_path_async`: the path as written if it exists, else the
/// unquoted path (Windows "Copy as path" wraps it in quotes, senpi#2170), else the macOS
/// screenshot AM/PM, NFD and curly-apostrophe spellings that exist, else the path as written.
pub fn resolve_read_path(file_path: &str, cwd: &Path) -> String {
    use unicode_normalization::UnicodeNormalization;
    let exists = |path: &str| std::fs::metadata(path).is_ok();
    let mut file_path = file_path.to_owned();
    let resolved = loop {
        let resolved = to_cwd(&file_path, cwd);
        if exists(&resolved) {
            return resolved;
        }
        match unquoted_path(&file_path) {
            Some(unquoted) => file_path = unquoted,
            None => break resolved,
        }
    };
    let mut am_pm = resolved.clone();
    for marker in ["AM", "PM", "am", "pm", "Am", "Pm", "aM", "pM"] {
        am_pm = am_pm.replace(&format!(" {marker}."), &format!("\u{202f}{marker}."));
    }
    let nfd: String = resolved.nfd().collect();
    let curly = resolved.replace('\'', "\u{2019}");
    let nfd_curly = nfd.replace('\'', "\u{2019}");
    [am_pm, nfd, curly, nfd_curly]
        .into_iter()
        .find(|candidate| *candidate != resolved && exists(candidate))
        .unwrap_or(resolved)
}
/// `^@?(["'])(.+)\1$` on the trimmed path.
fn unquoted_path(path: &str) -> Option<String> {
    let path = path.trim();
    let path = path.strip_prefix('@').unwrap_or(path);
    let quote = path
        .chars()
        .next()
        .filter(|quote| matches!(quote, '\'' | '"'))?;
    let inner = path.strip_prefix(quote)?.strip_suffix(quote)?;
    (!inner.is_empty()).then(|| inner.to_owned())
}

/// `read` (senpi `parseReadPermission`, `read-permission.rs`) plus the mhc credential check over
/// the file the read tool opens.
fn parse_read_permission(
    input: &Value,
    location: (&Path, &Path),
    agent_dir: &Path,
) -> Vec<PermissionRequest> {
    let Some(file_path) = string(input, &["path", "file_path"]).filter(|path| !path.is_empty())
    else {
        return fallback("read");
    };
    let requests = crate::read_permission::parse_read_permission(
        file_path,
        location,
        &crate::read_permission::bundled_resource_roots(agent_dir),
    );
    credential(
        requests,
        &[resolve_read_path(file_path, location.0)],
        location,
        agent_dir,
    )
}
