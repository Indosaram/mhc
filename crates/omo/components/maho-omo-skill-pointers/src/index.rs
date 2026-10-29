use fancy_regex::Regex as FancyRegex;
use maho_ext_api::*;
use regex::Regex;
use std::sync::{Arc, LazyLock};

use crate::strip_quoted_regions::{
    NOT_AFTER_IDENTIFIER, NOT_BEFORE_PATH, NOT_INTO_IDENTIFIER, strip_quoted_regions,
};

pub const MASS_ULW_CUSTOM_TYPE: &str = "omo-mass-ulw:skill-pointer";
pub const ULW_PLAN_CUSTOM_TYPE: &str = "omo-ulw-plan:skill-pointer";
pub const ULW_LOOP_CUSTOM_TYPE: &str = "omo-ulw-loop:skill-pointer";
pub const ULW_RESEARCH_CUSTOM_TYPE: &str = "omo-ulw-research:skill-pointer";
pub const BROWSER_CUSTOM_TYPE: &str = "omo-browser:skill-pointer";
pub const ULTIMATE_BROWSING_CUSTOM_TYPE: &str = "omo-ultimate-browsing:skill-pointer";
pub const SKILL_POINTERS_DISABLED_FLAG: &str = "omo-senpi-skill-pointers-disabled";

pub const SKILL_COMMAND_PREFIX: &str = "/skill:";

pub struct SkillCompanion {
    pub skill_name: &'static str,
    pub custom_type: &'static str,
    pub expanded_block_pattern: LazyLock<Regex>,
    pub role: &'static str,
    pub instruction: &'static str,
}

pub struct SkillPointerTarget {
    pub skill_name: &'static str,
    pub custom_type: &'static str,
    pub pattern: LazyLock<FancyRegex>,
    pub expanded_block_pattern: LazyLock<Regex>,
    pub instruction: &'static str,
    pub extra: Option<fn(Option<&str>) -> String>,
    pub companions: &'static [&'static SkillCompanion],
}

pub struct SkillPointerMessage {
    pub custom_type: &'static str,
    pub content: String,
}

pub static BROWSER_COMPANION: LazyLock<SkillCompanion> = LazyLock::new(|| SkillCompanion {
    skill_name: "browser",
    custom_type: BROWSER_CUSTOM_TYPE,
    expanded_block_pattern: LazyLock::new(|| match Regex::new(r#"(?i)<skill\s+name="browser""#) {
        Ok(regex) => regex,
        Err(error) => panic!("invalid browser skill block regex: {error}"),
    }),
    role: "reads its sources in the user's browser through the browser skill's attached engine",
    instruction: r#"arm every web lane with load_skills: ["browser"] and name that SKILL.md path in every web member brief, since team members carry no load_skills"#,
});

pub static ULTIMATE_BROWSING_COMPANION: LazyLock<SkillCompanion> = LazyLock::new(|| {
    SkillCompanion {
        skill_name: "ultimate-browsing",
        custom_type: ULTIMATE_BROWSING_CUSTOM_TYPE,
        expanded_block_pattern: LazyLock::new(|| {
            Regex::new(r#"(?i)<skill\s+name="ultimate-browsing""#).expect("valid regex")
        }),
        role: "sends the hosts the user's browser cannot open to the ultimate-browsing skill",
        instruction: r#"arm a lane that must reach a blocked host with load_skills: ["ultimate-browsing"]"#,
    }
});

static RESEARCH_COMPANIONS: LazyLock<[&SkillCompanion; 2]> =
    LazyLock::new(|| [&*BROWSER_COMPANION, &*ULTIMATE_BROWSING_COMPANION]);

/// JS `\b` is ASCII-only and every skill name starts and ends on an ASCII word character, so the
/// boundary is a lookaround; fancy-regex rejects `(?-u:\b)`.
const WORD_START: &str = r"(?<![A-Za-z0-9_])";
const WORD_END: &str = r"(?![A-Za-z0-9_])";

fn skill_name_pattern(body: &str) -> FancyRegex {
    match FancyRegex::new(&format!(
        r"(?i){NOT_AFTER_IDENTIFIER}{WORD_START}{body}{WORD_END}{NOT_INTO_IDENTIFIER}{NOT_BEFORE_PATH}"
    )) {
        Ok(regex) => regex,
        Err(error) => panic!("invalid skill name regex: {error}"),
    }
}

static MASS_ALIAS: &str = r"(?:mass[\s-]*ulw|ulw[\s-]*mass|mulw|meth)";

pub static TARGET_MASS_ULW: LazyLock<SkillPointerTarget> = LazyLock::new(|| SkillPointerTarget {
    skill_name: "mass-ulw",
    custom_type: MASS_ULW_CUSTOM_TYPE,
    pattern: LazyLock::new(|| skill_name_pattern(MASS_ALIAS)),
    expanded_block_pattern: LazyLock::new(|| {
        Regex::new(r#"(?i)<skill\s+name="mass-ulw""#).expect("valid regex")
    }),
    instruction: "dispatch each phase's dependency-ordered lanes as one run of the workflow tool composed in an eval cell, start a new run per phase rather than one graph for the whole job, and when a ulw-loop or ulw-execute contract is active let it own the goal",
    extra: None,
    companions: &[],
});

pub static TARGET_ULW_PLAN: LazyLock<SkillPointerTarget> = LazyLock::new(|| SkillPointerTarget {
    skill_name: "ulw-plan",
    custom_type: ULW_PLAN_CUSTOM_TYPE,
    pattern: LazyLock::new(|| skill_name_pattern(r"ulw[\s-]*plan")),
    expanded_block_pattern: LazyLock::new(|| {
        Regex::new(r#"(?i)<skill\s+name="ulw-plan""#).expect("valid regex")
    }),
    instruction: "run the explore-first planning workflow and produce one decision-complete work plan",
    extra: None,
    companions: &[],
});

pub static TARGET_ULW_LOOP: LazyLock<SkillPointerTarget> = LazyLock::new(|| SkillPointerTarget {
    skill_name: "ulw-loop",
    custom_type: ULW_LOOP_CUSTOM_TYPE,
    pattern: LazyLock::new(|| skill_name_pattern(r"ulw[\s-]*loop")),
    expanded_block_pattern: LazyLock::new(|| {
        Regex::new(r#"(?i)<skill\s+name="ulw-loop""#).expect("valid regex")
    }),
    instruction: "run the goal-driven ultrawork loop with evidence-bound execution",
    extra: Some(ulw_loop_tool_sentence),
    companions: &[],
});

pub static TARGET_ULW_RESEARCH: LazyLock<SkillPointerTarget> =
    LazyLock::new(|| SkillPointerTarget {
        skill_name: "ulw-research",
        custom_type: ULW_RESEARCH_CUSTOM_TYPE,
        pattern: LazyLock::new(|| {
            skill_name_pattern(&format!(r"(?:ulw|{})[\s-]*research", MASS_ALIAS))
        }),
        expanded_block_pattern: LazyLock::new(|| {
            Regex::new(r#"(?i)<skill\s+name="ulw-research""#).expect("valid regex")
        }),
        instruction: "orchestrate team-first maximum-saturation research",
        extra: None,
        companions: &*RESEARCH_COMPANIONS,
    });

pub fn get_targets() -> [&'static SkillPointerTarget; 4] {
    [
        &*TARGET_MASS_ULW,
        &*TARGET_ULW_PLAN,
        &*TARGET_ULW_LOOP,
        &*TARGET_ULW_RESEARCH,
    ]
}

pub fn binding_forms(command: &str, keyword: &str) -> FancyRegex {
    let lead = r"^\s*(?:\[(?:Image|Pasted text)\b[^\]\n]*\]\s*)*";
    let question = r"(?![\s\S]*[?？])";
    FancyRegex::new(&format!(r"(?i){lead}/(?:skill:)?{command}(?=\s|$){question}|{lead}(?:run|start|use|do|execute)\s+{keyword}(?=\s*(?:$|[,!:;\n\x00]|\.(?:\s|$)|(?:to|on|for|with|and|now|here)\b)){question}|{lead}{keyword}\s*(?:(?:으로|로|를|을)\s*)?(?:돌려줘|실행해줘|써줘|해줘|돌려|실행해)(?=\s*(?:[.!\n]|$)){question}")).expect("valid binding forms")
}

pub fn binds(target: &SkillPointerTarget, visible: &str) -> bool {
    const MASS: &str = r"(?:mass[\s-]*ulw|ulw[\s-]*mass|mulw)";
    static MASS_BIND: LazyLock<FancyRegex> = LazyLock::new(|| {
        binding_forms(r"(?:mass-ulw|mulw)", &format!(r"{MASS}(?:[\s-]*research)?"))
    });
    static RESEARCH_BIND: LazyLock<FancyRegex> = LazyLock::new(|| {
        binding_forms(
            r"(?:ulw-research|(?:mass-ulw|mulw)\s+research)",
            &format!(r"(?:ulw|{MASS})[\s-]*research"),
        )
    });
    match target.skill_name {
        "mass-ulw" => MASS_BIND.is_match(visible).unwrap_or(false),
        "ulw-research" => RESEARCH_BIND.is_match(visible).unwrap_or(false),
        _ => false,
    }
}

pub fn skill_mode(
    target: &SkillPointerTarget,
    mass_mode: bool,
    scope: Option<&str>,
    root: &str,
) -> String {
    let reads = if target.skill_name == "mass-ulw" {
        format!("{root}mass-ulw/SKILL.md and its references/planning.md")
    } else {
        format!("{root}{}/SKILL.md", target.skill_name)
    };
    let dispatch = if target.skill_name == "mass-ulw" {
        " The work runs as workflow runs started from an eval cell, their nodes executing in dependency-ordered waves; doing the job inline, or through one-off task spawns in place of workflow runs, does not satisfy this request. Where the ultrawork directive's general delegation guidance differs, this mode decides how lanes are dispatched."
    } else if mass_mode {
        " mass-ulw mode is active in the same message, so run the research waves as its workflow runs."
    } else {
        ""
    };
    let extra = target.extra.map(|f| f(scope)).unwrap_or_default();
    let name = target.skill_name;
    format!(
        "<omo-{name}-mode>The user of this session invoked {name} in their own words, so this turn runs in {name} mode. This is binding: the block is produced only for the skill's slash command or a prompt that opens with a request to run it. Before any other tool call this turn, read {reads} in full with the read tool, then follow the skill: {}.{dispatch}{extra}</omo-{name}-mode>",
        target.instruction
    )
}

pub fn matched_skill_pointer_names(text: &str) -> Vec<&'static str> {
    let visible = strip_quoted_regions(text);
    let mut names = Vec::new();
    for target in get_targets() {
        if target.pattern.is_match(&visible).unwrap_or(false) || binds(target, &visible) {
            names.push(target.skill_name);
        }
    }
    names
}

pub fn skill_command_name(text: &str) -> Option<&str> {
    if !text.starts_with(SKILL_COMMAND_PREFIX) {
        return None;
    }
    let rest = &text[SKILL_COMMAND_PREFIX.len()..];
    match rest.find(' ') {
        Some(idx) => Some(&rest[..idx]),
        None => Some(rest),
    }
}

pub fn normalize_ulw_loop_session_id(session_id: Option<&str>) -> Option<String> {
    let raw = session_id?.trim();
    if raw.is_empty() {
        return None;
    }
    let segments: Vec<&str> = raw
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .collect();
    let joined = if !segments.is_empty() {
        segments.join("-")
    } else {
        raw.to_string()
    };
    let mut mapped = String::with_capacity(joined.len());
    for c in joined.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
            mapped.push(c);
        } else {
            mapped.push('-');
        }
    }
    let mut collapsed = String::with_capacity(mapped.len());
    let mut last_dash = false;
    for c in mapped.chars() {
        if c == '-' {
            if !last_dash {
                collapsed.push('-');
                last_dash = true;
            }
        } else {
            collapsed.push(c);
            last_dash = false;
        }
    }
    let trimmed = collapsed.trim_matches(['.', '-']);
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub fn ulw_loop_tool_sentence(session_scope: Option<&str>) -> String {
    let base = " Drive every ulw-loop operation through the native CLI from bash: `mhc ulw-loop <subcommand> --json` (create-goals, status, complete-goals, checkpoint, steer, add-goal, criteria, record-evidence, record-review-blockers); the session's bash binds PI_SESSION_ID, so the CLI targets this session's plan. There is no JS SDK to import and no tool named omo_agent_toolkit.";
    match session_scope {
        Some(scope) => format!("{base} This session's state lives under .omo/ulw-loop/{scope}/."),
        None => base.to_string(),
    }
}

/// Whether `skill_name` ships under `skills_root`. A pointer to a skill whose directory is
/// gone (a retired skill such as `mass-ulw`) would send the model to a file that does not
/// exist, so it is withheld. When the root itself is not a readable directory the install
/// layout is unknown and the pointer is kept.
pub fn skill_is_shipped(skills_root: &str, skill_name: &str) -> bool {
    let root = std::path::Path::new(if skills_root.is_empty() { "skills" } else { skills_root });
    if !root.is_dir() {
        return true;
    }
    root.join(skill_name).join("SKILL.md").is_file()
}

pub fn skill_pointer(
    target: &SkillPointerTarget,
    session_scope: Option<&str>,
    skills_root: &str,
) -> String {
    let extra = match target.extra {
        Some(f) => f(session_scope),
        None => String::new(),
    };
    format!(
        "<omo-{}-pointer>This message mentions {}. If the user of this session is asking to run {}, read the {} skill at {}{}/SKILL.md with the read tool and follow it: {}. If {} is only being discussed, quoted, or relayed from another session, ignore this pointer.{}</omo-{}-pointer>",
        target.skill_name,
        target.skill_name,
        target.skill_name,
        target.skill_name,
        skills_root,
        target.skill_name,
        target.instruction,
        target.skill_name,
        extra,
        target.skill_name,
    )
}

pub fn companion_pointer(
    parent: &SkillPointerTarget,
    companion: &SkillCompanion,
    skills_root: &str,
) -> String {
    format!(
        "<omo-{}-pointer>{} {}. If the user of this session is asking to run {}, read the {} skill at {}{}/SKILL.md with the read tool in the same turn as the {} skill and {}. If {} is only being discussed, quoted, or relayed from another session, ignore this pointer.</omo-{}-pointer>",
        companion.skill_name,
        parent.skill_name,
        companion.role,
        parent.skill_name,
        companion.skill_name,
        skills_root,
        companion.skill_name,
        parent.skill_name,
        companion.instruction,
        parent.skill_name,
        companion.skill_name,
    )
}

#[derive(Clone, Debug)]
pub struct SkillPointersComponent {
    pub skills_root: String,
}

impl Default for SkillPointersComponent {
    fn default() -> Self {
        Self {
            skills_root: std::env::var("OMO_SENPI_SKILLS_ROOT")
                .ok()
                .filter(|root| !root.is_empty())
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    std::env::current_exe()
                        .ok()
                        .and_then(|exe| exe.parent().map(|parent| parent.join("skills")))
                })
                .unwrap_or_else(|| std::path::PathBuf::from("skills"))
                .to_string_lossy()
                .into_owned(),
        }
    }
}

impl SkillPointersComponent {
    pub fn new(skills_root: impl Into<String>) -> Self {
        Self {
            skills_root: skills_root.into(),
        }
    }

    pub fn normalized_skills_root(&self) -> String {
        if self.skills_root.is_empty() {
            "skills/".into()
        } else if self.skills_root.ends_with('/') {
            self.skills_root.clone()
        } else {
            format!("{}/", self.skills_root)
        }
    }
}

impl Extension for SkillPointersComponent {
    fn register(&self, api: &mut ExtensionApi) {
        let skills_root = self.normalized_skills_root();
        let role_delegated = api
            .profile
            .session_context
            .get("role")
            .is_some_and(|role| !role.is_empty());
        let runtime = api.runtime.clone();

        api.on(
            EventKind::Input,
            Arc::new(move |event, ctx| {
                let skills_root = skills_root.clone();
                let runtime = runtime.clone();
                let raw_session_id = ctx.session_manager.session_id();
                let session_scope = if raw_session_id.is_empty() {
                    None
                } else {
                    normalize_ulw_loop_session_id(Some(raw_session_id))
                };

                Box::pin(async move {
                    let ExtensionEvent::Input(input) = event else {
                        return Ok(EventResult::None);
                    };

                    if runtime.get_flag(SKILL_POINTERS_DISABLED_FLAG) == Some(FlagValue::Boolean(true)) {
                        return Ok(EventResult::Input(InputEventResult::Continue));
                    }

                    // A delegated session (task child, DAG child, team member) is driven by its brief:
                    // a skill its brief names is context for the work, never a request (#9740).
                    // Mirrors `readSessionRole`: the session context role wins, the env is the fallback.
                    let delegated = role_delegated
                        || std::env::var("SENPI_TASK_MEMBER").is_ok_and(|member| !member.is_empty())
                        || std::env::var("OMO_SENPI_TASK_RPC_CHILD").is_ok_and(|child| child == "1");
                    if input.source == InputSource::Extension || delegated {
                        return Ok(EventResult::Input(InputEventResult::Continue));
                    }

                    if input.text.is_empty() {
                        return Ok(EventResult::Input(InputEventResult::Continue));
                    }

                    let command_skill = skill_command_name(&input.text);
                    let visible = strip_quoted_regions(&input.text);

                    let targets = get_targets();
                    let invoked: Vec<&SkillPointerTarget> = targets
                        .into_iter()
                        .filter(|target| target.pattern.is_match(&visible).unwrap_or(false) || binds(target, &visible))
                        .filter(|target| skill_is_shipped(&skills_root, target.skill_name))
                        .collect();

                    let already_loaded = |skill_name: &str, expanded_block: &Regex| -> bool {
                        command_skill == Some(skill_name) || expanded_block.is_match(&input.text)
                    };

                    let mass_mode = invoked.iter().any(|target| target.skill_name == "mass-ulw" && binds(target, &visible));
                    let mut pointers: Vec<SkillPointerMessage> = invoked
                        .iter()
                        .filter(|target| !already_loaded(target.skill_name, &target.expanded_block_pattern))
                        .map(|target| SkillPointerMessage {
                            custom_type: target.custom_type,
                            content: if binds(target, &visible) { skill_mode(target, mass_mode, session_scope.as_deref(), &skills_root) } else { skill_pointer(target, session_scope.as_deref(), &skills_root) },
                        })
                        .collect();

                    for parent in invoked.iter() {
                        for companion in parent.companions.iter() {
                            if already_loaded(companion.skill_name, &companion.expanded_block_pattern) {
                                continue;
                            }
                            if pointers.iter().any(|p| p.custom_type == companion.custom_type) {
                                continue;
                            }
                            pointers.push(SkillPointerMessage {
                                custom_type: companion.custom_type,
                                content: if binds(parent, &visible) { format!("<omo-{}-pointer>{} {}. Read the {} skill at {}{}/SKILL.md with the read tool in the same turn as the {} skill and {}.</omo-{}-pointer>", companion.skill_name, parent.skill_name, companion.role, companion.skill_name, skills_root, companion.skill_name, parent.skill_name, companion.instruction, companion.skill_name) } else { companion_pointer(parent, companion, &skills_root) },
                            });
                        }
                    }

                    if pointers.is_empty() {
                        return Ok(EventResult::Input(InputEventResult::Continue));
                    }

                    if input.streaming_behavior.is_some() {
                        let mut parts = Vec::with_capacity(1 + pointers.len());
                        parts.push(input.text.clone());
                        for p in pointers {
                            parts.push(p.content);
                        }
                        return Ok(EventResult::Input(InputEventResult::Transform {
                            text: parts.join("\n"),
                            images: None,
                        }));
                    }

                    let client_api = ExtensionApi::new(
                        LoadedExtension::new("skill-pointers", std::path::PathBuf::new(), SourceInfo::default()),
                        ExtensionSessionProfile::default(),
                        EventBus::default(),
                        runtime,
                    );

                    for pointer in pointers {
                        client_api.send_message(
                            CustomMessage {
                                custom_type: pointer.custom_type.into(),
                                content: vec![ToolContent::text(pointer.content)],
                                display: false,
                                details: None,
                            },
                            SendMessageOptions::default(),
                        )?;
                    }

                    Ok(EventResult::Input(InputEventResult::Continue))
                })
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::matched_skill_pointer_names;

    #[test]
    fn positive_binding_forms_and_exclusions() {
        use super::*;
        for text in [
            "/mass-ulw fix",
            "/skill:mass-ulw fix",
            "run mass ulw on tests",
            "use mulw to ship",
            "mass ulw 돌려줘",
            "mass ulw로 써줘",
            "[Image #1] start ulw research now",
        ] {
            let visible = strip_quoted_regions(text);
            assert!(
                binds(&TARGET_MASS_ULW, &visible) || binds(&TARGET_ULW_RESEARCH, &visible),
                "{text}"
            );
        }
        for text in [
            "mass-ulw the fix",
            "run meth",
            "run mass ulw.ts again",
            "use mass ulw?",
            "use mass ulw？",
            "they run mass ulw",
            "'run mass ulw'",
            "[lead via coordinator -> lane, PR#1] run mass ulw",
            "don't use mass ulw",
        ] {
            let visible = strip_quoted_regions(text);
            assert!(!binds(&TARGET_MASS_ULW, &visible), "{text}");
        }
        let injected = skill_mode(&TARGET_MASS_ULW, true, None, "skills/");
        assert!(matched_skill_pointer_names(&injected).is_empty());
    }

    #[test]
    fn suppresses_identifier_path_quoted_relayed_and_negated_mentions() {
        let cases = [
            "what happened in the mass-ulw-refactor session?",
            "the senpi-ulw-loop lane finished",
            "notes are in .omo/ulw-plan/draft.md",
            "the mass-ulw-loop-runner session finished",
            "see mass-ulw-research-notes.md",
            "Do not load mass-ulw or ulw-research or launch your own workflow.",
            "[REPORT] lane | filed | mass ulw research and ulw-loop",
            "[a -> b] run ulw-loop",
            "Lead (main), lane: ulw-research and mass-ulw",
            "Lead (main) to duty: ulw-loop and mass ulw research",
            "they typed \u{201C}mass ulw research the market\u{201D} and it armed",
            "they typed \"ulw-loop\" and it armed",
            "> ulw-loop the migration\nshould this have armed?",
            "see https://example.com/ulw-loop",
            "ulw `not a request` loop",
            "review this log:\n```text\nulw-loop\n```",
            "~~~text\nulw-loop\n~~~",
            "````text\n```\nulw-loop\n````",
            "```text\nulw-loop",
        ];
        for text in cases {
            let matched = matched_skill_pointer_names(text);
            assert!(matched.is_empty(), "{text}: {matched:?}");
        }
    }

    #[test]
    fn matches_standalone_chains_and_requests_after_clause_breaks() {
        let cases = [
            ("mass-ulw the migration", vec!["mass-ulw"]),
            (
                "mass ulw-loop ship the refactor",
                vec!["mass-ulw", "ulw-loop"],
            ),
            ("ulw-plan.", vec!["ulw-plan"]),
            ("explain `ulw-plan`, then run ulw-loop", vec!["ulw-loop"]),
            ("Do not use ulw-plan, but run ulw-loop", vec!["ulw-loop"]),
            ("Don't load ulw-plan; run ulw-loop", vec!["ulw-loop"]),
            ("Never run ulw-plan then ulw-loop", vec!["ulw-loop"]),
            ("Do not arm ulw-plan - ulw-loop", vec!["ulw-loop"]),
        ];
        for (text, expected) in cases {
            let matched = matched_skill_pointer_names(text);
            assert_eq!(matched, expected, "{text}");
        }
    }
}
