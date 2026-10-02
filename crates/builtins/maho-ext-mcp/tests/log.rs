use maho_ext_mcp::log::*;
use serde_json::json;
#[test]
fn ring_and_file_are_redacted_before_write() {
    let root=tempfile::tempdir().unwrap();let mut logger=McpLogger::new("alpha",root.path(),None).unwrap();
    logger.log("info","request Authorization: Bearer abc123",None,None).unwrap();
    logger.log("info","https://example.test/mcp?api_key=url-secret-456",None,Some("stderr")).unwrap();
    logger.log("warn","payload",Some(&json!({"headers":{"Authorization":"Bearer abc123"},"body":{"client_secret":"json-secret-789"}})),None).unwrap();
    for text in [logger.get_ring_buffer().join("\n"),std::fs::read_to_string(&logger.file_path).unwrap()] {
        for secret in ["abc123","url-secret-456","json-secret-789"] {assert!(!text.contains(secret));assert!(text.contains(&format!("<redacted:{}>",fingerprint_secret(secret))));}
    }
}
#[test]
fn malformed_secret_bearing_text_is_masked() {
    let secret="malformed-token-value";let output=redact_mcp_log_text(&format!("Authorization: Bearer {secret}\n{{\"api_key\":\"{secret}\"}}\nhttps://example.test?client_secret={secret}")).unwrap();assert!(!output.contains(secret));
}
#[test]
fn basic_header_value_is_redacted() {assert_eq!(redact_mcp_log_text("Authorization: Basic abc123").unwrap(),format!("Authorization: Basic <redacted:{}>",fingerprint_secret("abc123")));}
#[test]
fn ring_is_capped_and_private_file_rotates() {
    let root=tempfile::tempdir().unwrap();let mut logger=McpLogger::new("rotation/server",root.path(),Some(640)).unwrap();
    for index in 0..260 {logger.log("debug",&format!("line {index:03}"),None,None).unwrap();}
    let ring=logger.get_ring_buffer();assert_eq!(ring.len(),200);assert!(ring[0].contains("line 060"));assert!(ring[199].contains("line 259"));assert!(std::path::PathBuf::from(format!("{}.1",logger.file_path.display())).exists());assert!(std::fs::metadata(&logger.file_path).unwrap().len()<=640);
    #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;assert_eq!(std::fs::metadata(&logger.file_path).unwrap().permissions().mode()&0o777,0o600);}
}
#[test]
fn failed_rotation_disables_sink_and_warns_once() {
    let root=tempfile::tempdir().unwrap();let mut logger=McpLogger::new("rotation",root.path(),Some(8)).unwrap();std::fs::write(&logger.file_path,"old line\n").unwrap();std::fs::create_dir(format!("{}.1",logger.file_path.display())).unwrap();
    logger.log("info","Authorization: Bearer fixture-token",None,None).unwrap();logger.log("info","next",None,None).unwrap();
    assert_eq!(logger.get_ring_buffer().iter().filter(|line|line.contains("file sink disabled")).count(),1);assert_eq!(std::fs::read_to_string(&logger.file_path).unwrap(),"old line\n");
}
#[test]
fn known_secret_is_removed_from_other_error_fields() {
    let root=tempfile::tempdir().unwrap();let mut logger=McpLogger::new("error",root.path(),None).unwrap();logger.log("error","failed",Some(&json!({"password":"fixture-secret","message":"request fixture-secret failed","stack":"Error fixture-secret"})),None).unwrap();assert!(!logger.get_ring_buffer().join("\n").contains("fixture-secret"));
}
#[test]
fn rfc_severity_mapping_matches_aliases() {assert_eq!(map_mcp_log_level("crit"),("critical",2));assert_eq!(map_mcp_log_level("informational"),("info",6));assert_eq!(map_mcp_log_level("unknown"),("info",6));}
