use maho_ext_permission_system::{events::PermissionEventEmitter, parsers::create_builtin_parser_registry, service::PermissionService, storage::{append_approved, load_approved}, types::*};
use serde_json::json;
use std::path::Path;

#[tokio::test]
async fn parsed_tools_persist_always_approval_across_service_reconstruction() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let outcome = async {
        for (name, input, permission) in [
            ("bash", json!({"command":"git commit"}), "bash"),
            ("write", json!({"path":"src/file.rs"}), "edit"),
            ("apply_patch", json!({"path":"src/file.rs"}), "edit"),
            ("multiedit", json!({"path":"src/file.rs"}), "edit"),
        ] {
            let parsed = create_builtin_parser_registry().parse(name, &input, (directory.path(), Path::new("/home/test")));
            assert_eq!(parsed.len(), 1);
            assert_eq!(parsed[0].permission, permission);
            let request = Request { id: name.into(), session_id: "original".into(), permission: parsed[0].permission.clone(), patterns: parsed[0].patterns.clone(), always: parsed[0].always.clone(), metadata: Default::default(), tool: None };
            let mut service = PermissionService::new(vec![], vec![], PermissionEventEmitter::default());
            let completion = service.ask(request.clone());
            assert_eq!(serde_json::to_value(service.list())?, serde_json::to_value(vec![request.clone()])?);
            service.reply(ReplyInput { request_id: name.into(), reply: Reply::Always, message: None });
            completion.await?;
            let approved = service.get_approved();
            assert_eq!(approved, request.always.iter().map(|pattern| Rule { permission: permission.into(), pattern: pattern.clone(), action: Action::Allow }).collect::<Vec<_>>());
            append_approved(directory.path(), &approved)?;
            let persisted = load_approved(directory.path())?;
            assert!(approved.iter().all(|rule| persisted.contains(rule)));
            let mut reopened = PermissionService::new(vec![], persisted, PermissionEventEmitter::default());
            let mut next = request;
            next.id = format!("{name}-next");
            next.session_id = "reconstructed".into();
            reopened.ask(next).await?;
            assert!(reopened.list().is_empty());
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    }.await;
    directory.close()?;
    outcome
}
