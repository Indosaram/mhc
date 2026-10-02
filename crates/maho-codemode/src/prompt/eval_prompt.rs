use std::collections::HashMap;
use crate::{config::settings::{Languages, DEFAULT_RUN_BUDGET_SECONDS, DEFAULT_MAX_DETACHED_CELLS}, tool::types::EvalRuntimeInfo};
use super::eval_prompt_template::EVAL_PROMPT_TEMPLATE;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvalEmphasisStyle { Default, Claude, Codex, Gpt, Kimi }

pub fn is_gpt_code_mode_model(model_id: Option<&str>) -> bool {
    model_id.is_some_and(|id| regex::Regex::new(r"(?i)(^|[/.:])gpt[-.]").expect("model pattern").is_match(id))
}

pub fn eval_emphasis_style(model_id: Option<&str>) -> EvalEmphasisStyle {
    let Some(id) = model_id.filter(|id| !id.is_empty()) else { return EvalEmphasisStyle::Default; };
    if is_gpt_code_mode_model(Some(id)) { return EvalEmphasisStyle::Gpt; }
    for (pattern, style) in [
        (r"(?i)(^|[/.:])claude[-.]|(^|[/.:@-])glm[-.]?\d", EvalEmphasisStyle::Claude),
        (r"(?i)(^|[/.:])kimi[-.]", EvalEmphasisStyle::Kimi),
        (r"(?i)(^|[/.:])(gpt|chatgpt|codex)[-.]|(^|[/.:])o[134]([-.]|$)", EvalEmphasisStyle::Codex),
    ] { if regex::Regex::new(pattern).expect("model pattern").is_match(id) { return style; } }
    EvalEmphasisStyle::Default
}

#[derive(Default)]
pub struct EvalPromptOptions {
    pub spawns: bool,
    pub monitor: bool,
    pub spawn_default_agent: Option<String>,
    pub model_id: Option<String>,
    pub host_line: Option<String>,
    pub js_runtime: Option<EvalRuntimeInfo>,
    pub bun_skill_path: Option<String>,
    pub run_budget_seconds: Option<f64>,
    pub max_detached_cells: Option<f64>,
}

pub struct EvalPromptParts {
    pub description: String,
    pub prompt_snippet: String,
    pub prompt_guidelines: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct EvalPromptError(pub String);

pub fn build_eval_prompt(enabled: &Languages, options: &EvalPromptOptions) -> Result<EvalPromptParts, EvalPromptError> {
    if !enabled.py && !enabled.js && !enabled.rb && !enabled.jl { return Err(EvalPromptError("no kernels enabled for eval prompt".into())); }
    let style = eval_emphasis_style(options.model_id.as_deref());
    let mut context = HashMap::new();
    for (key, value) in [
        ("py", enabled.py), ("js", enabled.js), ("rb", enabled.rb), ("jl", enabled.jl),
        ("spawns", options.spawns), ("monitor", options.monitor),
        ("styleClaude", style == EvalEmphasisStyle::Claude), ("styleCodex", style == EvalEmphasisStyle::Codex),
        ("styleGpt", style == EvalEmphasisStyle::Gpt), ("styleKimi", style == EvalEmphasisStyle::Kimi),
        ("styleDefault", style == EvalEmphasisStyle::Default),
        ("jsBun", options.js_runtime.as_ref().is_some_and(|runtime| runtime.name == "bun")),
    ] { context.insert(key.to_owned(), serde_json::Value::Bool(value)); }
    for (key, value) in [
        ("spawnDefaultAgent", options.spawn_default_agent.as_deref().unwrap_or("task").to_owned()),
        ("hostLine", options.host_line.clone().unwrap_or_default()),
        ("jsVersion", options.js_runtime.as_ref().map_or_else(String::new, |runtime| runtime.version.clone())),
        ("bunSkillPath", options.bun_skill_path.clone().unwrap_or_default()),
        ("runBudgetSeconds", options.run_budget_seconds.unwrap_or(DEFAULT_RUN_BUDGET_SECONDS).to_string()),
        ("maxDetachedCells", options.max_detached_cells.unwrap_or(DEFAULT_MAX_DETACHED_CELLS).to_string()),
    ] { context.insert(key.to_owned(), serde_json::Value::String(value)); }
    let (rendered, index, _) = render_until(EVAL_PROMPT_TEMPLATE, &context, 0, &[])?;
    if index != EVAL_PROMPT_TEMPLATE.len() { return Err(EvalPromptError("unexpected template close tag".into())); }
    let description = regex::Regex::new(r"\n{3,}").expect("newline pattern").replace_all(&rendered, "\n\n").trim().to_owned();
    let guideline = match style {
        EvalEmphasisStyle::Gpt if options.monitor => "Use eval to compose tool work in one cell; a wait or a long run starts through `tool.monitor` in that cell, so no cell sits on it and nothing polls.",
        EvalEmphasisStyle::Default => "Prefer eval when a step's calls are independent: one cell runs them together and keeps every failure in its result; edits and result-dependent calls go one at a time, each observed before the next.",
        EvalEmphasisStyle::Claude => "Prefer eval for a step's independent calls: one cell runs them together and keeps every failure in its result.",
        EvalEmphasisStyle::Codex => "Route a step's independent calls through one eval cell and inspect every result; a direct tool call is right when one call is sufficient.",
        EvalEmphasisStyle::Gpt => "Use eval to batch a step's independent tool calls in one cell and inspect every result; long cells detach on their own and notify on completion, so do not poll.",
        EvalEmphasisStyle::Kimi => "Put a step's independent calls into one eval cell with parallel(thunks) and keep every failed item in the result.",
    };
    Ok(EvalPromptParts { description, prompt_snippet: "Run one incremental code cell in a persistent language kernel.".into(), prompt_guidelines: vec![guideline.into(), "Use eval reset only when a language kernel must be wiped; reset is scoped to the selected language.".into()] })
}

fn truthy(value: Option<&serde_json::Value>) -> bool {
    match value { Some(serde_json::Value::Bool(value)) => *value, Some(serde_json::Value::String(value)) => !value.is_empty(), _ => false }
}

fn render_until(template: &str, context: &HashMap<String, serde_json::Value>, start: usize, stop_tags: &[&str]) -> Result<(String, usize, Option<String>), EvalPromptError> {
    let mut output = String::new();
    let mut index = start;
    while index < template.len() {
        let Some(open) = template[index..].find("{{").map(|open| open + index) else { output.push_str(&template[index..]); return Ok((output, template.len(), None)); };
        output.push_str(&template[index..open]);
        let close = template[open + 2..].find("}}").map(|close| close + open + 2).ok_or_else(|| EvalPromptError("unterminated template tag".into()))?;
        let tag = template[open + 2..close].trim();
        index = close + 2;
        if stop_tags.contains(&tag) { return Ok((output, index, Some(tag.into()))); }
        if let Some(open_tag) = tag.strip_prefix('#') {
            let mut names = open_tag.split_whitespace();
            let kind = names.next().unwrap_or("");
            let names: Vec<_> = names.collect();
            let close_tag = format!("/{kind}");
            let (truthy_text, after_truthy, stop_tag) = render_until(template, context, index, &["else", &close_tag])?;
            let (falsey_text, end) = if stop_tag.as_deref() == Some("else") {
                let (text, end, stop) = render_until(template, context, after_truthy, &[&close_tag])?;
                if stop.as_deref() != Some(&close_tag) { return Err(EvalPromptError(format!("missing close tag for {kind}"))); }
                (text, end)
            } else {
                if stop_tag.as_deref() != Some(&close_tag) { return Err(EvalPromptError(format!("missing close tag for {kind}"))); }
                (String::new(), after_truthy)
            };
            let condition = match kind {
                "if" => names.len() == 1 && truthy(context.get(names[0])),
                "ifAll" => !names.is_empty() && names.iter().all(|name| truthy(context.get(*name))),
                "ifAny" => !names.is_empty() && names.iter().any(|name| truthy(context.get(*name))),
                _ => return Err(EvalPromptError(format!("unknown template condition {kind}"))),
            };
            output.push_str(if condition { &truthy_text } else { &falsey_text });
            index = end;
        } else if tag.starts_with('/') { return Err(EvalPromptError(format!("unexpected template close tag {tag}"))); }
        else if let Some(serde_json::Value::String(value)) = context.get(tag) { output.push_str(value); }
    }
    Ok((output, index, None))
}
