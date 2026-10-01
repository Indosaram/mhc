use maho_ext_mcp::guard::output_guard::*;
use serde_json::{json,Value};
use base64::{Engine,engine::general_purpose::STANDARD};
fn spill_path(content:&[Value])->std::path::PathBuf {content[0]["text"].as_str().expect("text result").lines().find_map(|line|line.strip_prefix("Full output saved to: ")).expect("spill path").into()}
#[test]
fn huge_text_spills_exact_bytes_with_bounded_preview() {
    let root=tempfile::tempdir().unwrap();let owner=McpOutputArtifacts::default();let text="x".repeat(1048576);
    let result=apply_mcp_output_guard(&[json!({"type":"text","text":text})],McpOutputGuardOptions {agent_dir:root.path(),artifacts:Some(&owner),server:"fx",output_guard:None});
    let path=spill_path(&result);assert_eq!(std::fs::read_to_string(&path).unwrap(),text);assert!(result[0]["text"].as_str().unwrap().len()<10000);
    #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o777,0o600);}
}
#[test]
fn small_content_passes_through_unchanged() {
    let root=tempfile::tempdir().unwrap();let content=vec![json!({"type":"text","text":"small"})];
    assert_eq!(apply_mcp_output_guard(&content,McpOutputGuardOptions {agent_dir:root.path(),artifacts:None,server:"fx",output_guard:None}),content);
    assert!(!root.path().join("tmp").exists());
}
#[test]
fn image_spill_preserves_binary_and_extension() {
    let root=tempfile::tempdir().unwrap();let owner=McpOutputArtifacts::default();let bytes=vec![0x89;65536];
    let result=apply_mcp_output_guard(&[json!({"type":"image","data":STANDARD.encode(&bytes),"mimeType":"image/png"})],McpOutputGuardOptions {agent_dir:root.path(),artifacts:Some(&owner),server:"fx",output_guard:None});
    let path=spill_path(&result);assert_eq!(path.extension().unwrap(),"png");assert_eq!(std::fs::read(path).unwrap(),bytes);
}
#[test]
fn spills_have_unique_paths() {
    let root=tempfile::tempdir().unwrap();let owner=McpOutputArtifacts::default();let content=vec![json!({"type":"text","text":"x".repeat(65536)})];
    let a=apply_mcp_output_guard(&content,McpOutputGuardOptions {agent_dir:root.path(),artifacts:Some(&owner),server:"fx",output_guard:None});
    let b=apply_mcp_output_guard(&content,McpOutputGuardOptions {agent_dir:root.path(),artifacts:Some(&owner),server:"fx",output_guard:None});
    assert_ne!(spill_path(&a),spill_path(&b));
}
#[test]
fn owner_cleanup_removes_its_spills() {
    let root=tempfile::tempdir().unwrap();let owner=McpOutputArtifacts::default();
    let result=apply_mcp_output_guard(&[json!({"type":"text","text":"x".repeat(65536)})],McpOutputGuardOptions {agent_dir:root.path(),artifacts:Some(&owner),server:"fx",output_guard:None});
    let path=spill_path(&result);owner.cleanup().unwrap();assert!(!path.exists());
}
#[test]
fn write_failure_returns_inline_preview() {
    let root=tempfile::tempdir().unwrap();std::fs::write(root.path().join("tmp"),"not directory").unwrap();
    let result=apply_mcp_output_guard(&[json!({"type":"text","text":"x".repeat(65536)})],McpOutputGuardOptions {agent_dir:root.path(),artifacts:None,server:"fx",output_guard:None});
    assert!(!root.path().join("tmp/mcp-out").exists());assert!(result[0]["text"].as_str().unwrap().len()<10000);
}
