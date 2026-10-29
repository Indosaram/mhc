//! The child's framed first prompt (`runners/in-process/subagent-prompt.ts`).

pub struct SubagentPromptInput<'a> {
    pub task_id: &'a str,
    pub parent_session_id: &'a str,
    pub root_session_id: &'a str,
    pub depth: u32,
    pub agent_type: Option<&'a str>,
    pub instructions: Option<&'a str>,
    pub prompt: &'a str,
}

pub fn build_subagent_prompt(input: &SubagentPromptInput<'_>) -> String {
    let agent = input
        .agent_type
        .filter(|agent| !agent.is_empty())
        .map(|agent| format!(" \"{agent}\""))
        .unwrap_or_default();
    let mut lines = vec![
        format!("You are running as an omo senpi-task child{agent}."),
        format!("Task id: {}.", input.task_id),
        format!("Parent session: {}.", input.parent_session_id),
        format!("Root session: {}.", input.root_session_id),
        format!("Depth: {}.", input.depth),
    ];
    if let Some(instructions) = input
        .instructions
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        lines.extend([
            String::new(),
            "Instructions:".to_string(),
            instructions.to_string(),
        ]);
    }
    lines.extend([String::new(), "Task:".to_string(), input.prompt.to_string()]);
    lines.join("\n")
}
