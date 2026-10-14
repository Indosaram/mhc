use maho_ext_api::*;
use regex::Regex;
use std::sync::{Arc, LazyLock};

use crate::strip_quoted_regions::strip_quoted_regions;

pub const MASS_ULW_CUSTOM_TYPE: &str = "omo-mass-ulw:skill-pointer";
pub const ULW_PLAN_CUSTOM_TYPE: &str = "omo-ulw-plan:skill-pointer";
pub const ULW_LOOP_CUSTOM_TYPE: &str = "omo-ulw-loop:skill-pointer";
pub const ULW_RESEARCH_CUSTOM_TYPE: &str = "omo-ulw-research:skill-pointer";
pub const ULTIMATE_BROWSING_CUSTOM_TYPE: &str = "omo-ultimate-browsing:skill-pointer";
pub const SKILL_POINTERS_DISABLED_FLAG: &str = "omo-senpi-skill-pointers-disabled";
/// Pin `77f3067f1` `components/mass-ulw/index.ts:6`: the pinned mass-ulw component's disable flag.
/// Latest folds mass-ulw into this component, so the pinned flag suppresses only the mass-ulw target.
pub const MASS_ULW_DISABLED_FLAG: &str = "omo-senpi-mass-ulw-disabled";

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
    pub pattern: LazyLock<Regex>,
    pub expanded_block_pattern: LazyLock<Regex>,
    pub instruction: &'static str,
    pub extra: Option<fn(Option<&str>) -> String>,
    pub companions: &'static [SkillCompanion],
}

pub struct SkillPointerMessage {
    pub custom_type: &'static str,
    pub content: String,
}

pub static ULTIMATE_BROWSING_COMPANION: LazyLock<SkillCompanion> = LazyLock::new(|| SkillCompanion {
    skill_name: "ultimate-browsing",
    custom_type: ULTIMATE_BROWSING_CUSTOM_TYPE,
    expanded_block_pattern: LazyLock::new(|| {
        Regex::new(r#"(?i)<skill\s+name="ultimate-browsing""#).expect("valid regex")
    }),
    role: "runs its browsing lanes on the ultimate-browsing skill",
    instruction: r#"arm every browsing lane or member with load_skills: ["ultimate-browsing"]"#,
});

static MASS_ALIAS: &str = r"(?:mass[\s-]*ulw|ulw[\s-]*mass|mulw|meth)";

pub static TARGET_MASS_ULW: LazyLock<SkillPointerTarget> = LazyLock::new(|| SkillPointerTarget {
    skill_name: "mass-ulw",
    custom_type: MASS_ULW_CUSTOM_TYPE,
    pattern: LazyLock::new(|| {
        let pat = format!(r"(?i)(?-u:\b){}(?-u:\b)", MASS_ALIAS);
        Regex::new(&pat).expect("valid regex")
    }),
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
    pattern: LazyLock::new(|| {
        Regex::new(r"(?i)(?-u:\b)ulw[\s-]*plan(?-u:\b)").expect("valid regex")
    }),
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
    pattern: LazyLock::new(|| {
        Regex::new(r"(?i)(?-u:\b)ulw[\s-]*loop(?-u:\b)").expect("valid regex")
    }),
    expanded_block_pattern: LazyLock::new(|| {
        Regex::new(r#"(?i)<skill\s+name="ulw-loop""#).expect("valid regex")
    }),
    instruction: "run the goal-driven ultrawork loop with evidence-bound execution",
    extra: Some(ulw_loop_tool_sentence),
    companions: &[],
});

pub static TARGET_ULW_RESEARCH: LazyLock<SkillPointerTarget> = LazyLock::new(|| SkillPointerTarget {
    skill_name: "ulw-research",
    custom_type: ULW_RESEARCH_CUSTOM_TYPE,
    pattern: LazyLock::new(|| {
        let pat = format!(r"(?i)(?-u:\b)(?:ulw|{})[\s-]*research(?-u:\b)", MASS_ALIAS);
        Regex::new(&pat).expect("valid regex")
    }),
    expanded_block_pattern: LazyLock::new(|| {
        Regex::new(r#"(?i)<skill\s+name="ulw-research""#).expect("valid regex")
    }),
    instruction: "orchestrate team-first maximum-saturation research",
    extra: None,
    companions: std::slice::from_ref(&*ULTIMATE_BROWSING_COMPANION),
});

pub fn get_targets() -> [&'static SkillPointerTarget; 4] {
    [
        &*TARGET_MASS_ULW,
        &*TARGET_ULW_PLAN,
        &*TARGET_ULW_LOOP,
        &*TARGET_ULW_RESEARCH,
    ]
}

pub fn matched_skill_pointer_names(text: &str) -> Vec<&'static str> {
    let visible = strip_quoted_regions(text);
    let mut names = Vec::new();
    for target in get_targets() {
        if target.pattern.is_match(&visible) {
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
            skills_root: std::env::var("OMO_SKILLS_ROOT")
                .or_else(|_| std::env::var("SENPI_SKILLS_ROOT"))
                .unwrap_or_else(|_| "skills/".into()),
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
        api.register_flag(
            MASS_ULW_DISABLED_FLAG,
            FlagType::Boolean { default: Some(false) },
            Some("Disable the omo-senpi mass-ulw skill pointer.".to_owned()),
        );
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

                    if input.source == InputSource::Extension {
                        return Ok(EventResult::Input(InputEventResult::Continue));
                    }

                    if input.text.is_empty() {
                        return Ok(EventResult::Input(InputEventResult::Continue));
                    }

                    let command_skill = skill_command_name(&input.text);
                    let visible = strip_quoted_regions(&input.text);

                    let mass_ulw_disabled =
                        runtime.get_flag(MASS_ULW_DISABLED_FLAG) == Some(FlagValue::Boolean(true));
                    let targets = get_targets();
                    let invoked: Vec<&SkillPointerTarget> = targets
                        .into_iter()
                        .filter(|target| !(mass_ulw_disabled && target.skill_name == TARGET_MASS_ULW.skill_name))
                        .filter(|target| target.pattern.is_match(&visible))
                        .collect();

                    let already_loaded = |skill_name: &str, expanded_block: &Regex| -> bool {
                        command_skill == Some(skill_name) || expanded_block.is_match(&input.text)
                    };

                    let mut pointers: Vec<SkillPointerMessage> = invoked
                        .iter()
                        .filter(|target| !already_loaded(target.skill_name, &target.expanded_block_pattern))
                        .map(|target| SkillPointerMessage {
                            custom_type: target.custom_type,
                            content: skill_pointer(target, session_scope.as_deref(), &skills_root),
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
                                content: companion_pointer(parent, companion, &skills_root),
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
