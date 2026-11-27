//! Port of `packages/coding-agent/src/cli/held-session-attach.ts` (senpi#6911).

use std::path::Path;

use maho_ai::types::ImageContent;

use super::args::{Args, is_valid_thinking_level};
use super::file_processor::process_file_arguments;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachModel {
    pub provider: String,
    pub model_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AttachPrompt {
    pub text: String,
    pub images: Vec<ImageContent>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AttachStartup {
    pub model: Option<AttachModel>,
    pub thinking: Option<String>,
    pub prompts: Vec<AttachPrompt>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HeldSessionAttachPlan {
    Startup(AttachStartup),
    Refusal(String),
}

fn unapplied_flags(parsed: &Args) -> Vec<String> {
    let mut flags = Vec::new();
    let checks: [(&str, bool); 24] = [
        ("--system-prompt", parsed.system_prompt.is_some()),
        (
            "--append-system-prompt",
            !parsed.append_system_prompt.is_empty(),
        ),
        ("--api-key", parsed.api_key.is_some()),
        ("--models", parsed.models.is_some()),
        ("--tools", parsed.tools.is_some()),
        ("--exclude-tools", parsed.exclude_tools.is_some()),
        ("--no-tools", parsed.no_tools),
        ("--no-builtin-tools", parsed.no_builtin_tools),
        ("--tool-runtime", parsed.tool_runtime.is_some()),
        ("--extension", !parsed.extensions.is_empty()),
        ("--no-extensions", parsed.no_extensions),
        ("--skill", !parsed.skills.is_empty()),
        ("--no-skills", parsed.no_skills),
        ("--prompt-template", !parsed.prompt_templates.is_empty()),
        ("--no-prompt-templates", parsed.no_prompt_templates),
        ("--theme", !parsed.themes.is_empty()),
        ("--no-themes", parsed.no_themes),
        ("--no-context-files", parsed.no_context_files),
        ("--name", parsed.name.is_some()),
        ("--offline", parsed.offline),
        (
            "--approve/--no-approve",
            parsed.project_trust_override.is_some(),
        ),
        ("--verbose", parsed.verbose),
        ("--grok-neo", parsed.grok_neo),
        ("- (stdin)", parsed.read_stdin),
    ];
    for (flag, given) in checks {
        if given {
            flags.push(flag.to_owned());
        }
    }
    for flag in parsed.unknown_flags.keys() {
        flags.push(format!("--{flag}"));
    }
    flags
}

fn refusal(reason: String) -> HeldSessionAttachPlan {
    HeldSessionAttachPlan::Refusal(format!(
        "{reason} This session runs in a multi-session host, so this terminal attaches to it and applies the command line through the session's own commands."
    ))
}

/// The command line of a terminal start that attaches to a host-held session (senpi#6911):
/// its prompts, `@file` arguments, `--model` and `--thinking` become the attached session's own
/// commands, never dropped silently. What cannot be applied exactly (a model pattern without a
/// provider) refuses the start instead.
pub async fn held_session_attach_plan(
    parsed: &Args,
    cwd: &Path,
    auto_resize_images: bool,
) -> Result<HeldSessionAttachPlan, String> {
    let unapplied = unapplied_flags(parsed);
    if !unapplied.is_empty() {
        let pronoun = if unapplied.len() == 1 { "it" } else { "them" };
        return Ok(refusal(format!(
            "{} cannot be applied to a session another process already runs; start without {pronoun}.",
            unapplied.join(", "),
        )));
    }
    let mut thinking = parsed.thinking.clone();
    let mut model = None;
    if let Some(raw_model) = &parsed.model {
        let (provider, mut model_id) = match &parsed.provider {
            Some(provider) => (provider.clone(), raw_model.clone()),
            None => match raw_model.find('/') {
                Some(slash) if slash > 0 => (
                    raw_model[..slash].to_owned(),
                    raw_model[slash + 1..].to_owned(),
                ),
                _ => {
                    return Ok(refusal(format!(
                        "--model {raw_model} names no provider: use --provider <name> --model <id> or --model <provider>/<id>."
                    )));
                }
            },
        };
        if let Some(colon) = model_id.rfind(':')
            && colon > 0
        {
            let suffix = &model_id[colon + 1..];
            if is_valid_thinking_level(suffix) {
                if thinking.is_none() {
                    thinking = Some(suffix.to_owned());
                }
                model_id.truncate(colon);
            }
        }
        if model_id.is_empty() || model_id.contains('*') {
            return Ok(refusal(format!(
                "--model {raw_model} is a pattern; name one model as <provider>/<id>."
            )));
        }
        model = Some(AttachModel { provider, model_id });
    } else if let Some(provider) = &parsed.provider {
        return Ok(refusal(format!("--provider {provider} needs --model.")));
    }
    let prompts = attach_prompts(parsed, cwd, auto_resize_images).await?;
    Ok(HeldSessionAttachPlan::Startup(AttachStartup {
        model,
        thinking,
        prompts,
    }))
}

/// The first prompt carries the `@file` text and images before the first message, as an in-process start sends it.
async fn attach_prompts(
    parsed: &Args,
    cwd: &Path,
    auto_resize_images: bool,
) -> Result<Vec<AttachPrompt>, String> {
    let Some((first, rest)) = parsed.messages.split_first() else {
        if parsed.file_args.is_empty() {
            return Ok(Vec::new());
        }
        let processed =
            process_file_arguments(&parsed.file_args, cwd, Some(auto_resize_images)).await?;
        return Ok(vec![AttachPrompt {
            text: processed.text,
            images: processed.images,
        }]);
    };
    let rest_prompts = rest.iter().map(|text| AttachPrompt {
        text: text.clone(),
        images: Vec::new(),
    });
    if parsed.file_args.is_empty() {
        let mut prompts = Vec::with_capacity(1 + rest.len());
        prompts.push(AttachPrompt {
            text: first.clone(),
            images: Vec::new(),
        });
        prompts.extend(rest_prompts);
        return Ok(prompts);
    }
    let processed =
        process_file_arguments(&parsed.file_args, cwd, Some(auto_resize_images)).await?;
    let mut prompts = Vec::with_capacity(1 + rest.len());
    prompts.push(AttachPrompt {
        text: format!("{}{first}", processed.text),
        images: processed.images,
    });
    prompts.extend(rest_prompts);
    Ok(prompts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn translates_model_thinking_and_prompts_or_refuses_unapplied_flags() {
        let mut args = Args {
            model: Some("openai/gpt-5:high".to_owned()),
            messages: vec!["hello".to_owned(), "second".to_owned()],
            ..Default::default()
        };
        let plan = held_session_attach_plan(&args, Path::new("."), true)
            .await
            .expect("plan");
        assert_eq!(
            plan,
            HeldSessionAttachPlan::Startup(AttachStartup {
                model: Some(AttachModel {
                    provider: "openai".to_owned(),
                    model_id: "gpt-5".to_owned(),
                }),
                thinking: Some("high".to_owned()),
                prompts: vec![
                    AttachPrompt {
                        text: "hello".to_owned(),
                        images: Vec::new()
                    },
                    AttachPrompt {
                        text: "second".to_owned(),
                        images: Vec::new()
                    },
                ],
            })
        );

        args.system_prompt = Some("custom".to_owned());
        let refused = held_session_attach_plan(&args, Path::new("."), true)
            .await
            .expect("refused");
        assert!(
            matches!(refused, HeldSessionAttachPlan::Refusal(ref msg) if msg.contains("--system-prompt"))
        );
    }
}
