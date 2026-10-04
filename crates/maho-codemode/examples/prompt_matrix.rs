use maho_codemode::{config::settings::Languages,prompt::eval_prompt::{build_eval_prompt,EvalPromptOptions}};

fn main()->Result<(),Box<dyn std::error::Error>> {
    for mask in 1..16 {
        for model in [None,Some("claude-5"),Some("codex-1"),Some("gpt-6"),Some("kimi-k2")] {
            for monitor in [false,true] {
                for spawns in [false,true] {
                    let enabled=Languages {py:mask&1!=0,js:mask&2!=0,rb:mask&4!=0,jl:mask&8!=0};
                    let options=EvalPromptOptions {model_id:model.map(str::to_owned),monitor,spawns,..Default::default()};
                    let result=build_eval_prompt(&enabled,&options)?;
                    println!("{}",serde_json::json!({"mask":mask,"model":model,"monitor":monitor,"spawns":spawns,"description":result.description,"promptSnippet":result.prompt_snippet,"promptGuidelines":result.prompt_guidelines}));
                }
            }
        }
    }
    Ok(())
}
