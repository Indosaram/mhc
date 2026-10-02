use maho_ext_pi_comment_checker::{cli::{RunStatus, run_checker}, core::{CommentCheckRequest, to_hook_input}};
use comment_checker_core::HookToolInput;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let binary = std::env::args_os().nth(1).ok_or("binary path required")?;
    let request = CommentCheckRequest {
        source_tool_name: "write".into(), tool_name: "Write".into(), file_path: "fixture.py".into(),
        tool_input: HookToolInput { file_path: Some("fixture.py".into()), content: Some("# Set the value to one\nvalue = 1\n".into()), ..Default::default() },
    };
    let input = to_hook_input(&request, "fixture-session", "/tmp");
    let result = run_checker(&input, Some(std::path::Path::new(&binary))).await;
    println!("status={:?}\n{}", result.status, result.message);
    if result.status != RunStatus::Warning { return Err("expected redundant-comment warning".into()); }
    Ok(())
}
