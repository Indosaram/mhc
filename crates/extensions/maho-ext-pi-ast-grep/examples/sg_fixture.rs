use maho_ext_pi_ast_grep::cli::{RunSgOptions, run_sg};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let binary = std::env::args_os().nth(1).ok_or("binary path required")?;
    let fixture = tempfile::tempdir()?;
    let file = fixture.path().join("fixture.ts");
    std::fs::write(&file, "console.log(value);\n")?;
    let mut options = RunSgOptions { pattern: "console.log($MSG)".into(), lang: "typescript".into(), paths: vec![file.to_string_lossy().into_owned()], rewrite: Some("logger.info($MSG)".into()), ..Default::default() };
    let binary = std::path::Path::new(&binary);
    let preview = run_sg(&options, binary).await;
    if preview.matches.len() != 1 || preview.error.is_some() { return Err(format!("preview failed: {preview:?}").into()); }
    if std::fs::read_to_string(&file)? != "console.log(value);\n" { return Err("preview modified fixture".into()); }
    options.update_all = true;
    let applied = run_sg(&options, binary).await;
    if applied.matches.len() != 1 || applied.error.is_some() { return Err(format!("apply failed: {applied:?}").into()); }
    if std::fs::read_to_string(&file)? != "logger.info(value);\n" { return Err("apply did not rewrite fixture".into()); }
    println!("preview_matches=1 preview_unchanged=true applied_matches=1 rewrite_verified=true");
    Ok(())
}
