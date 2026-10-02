use maho_core::settings_manager::{SettingsManager,InMemorySettingsStorage,SettingsStorage,SettingsScope};
use maho_ext_permission_system::{settings::load_permission_settings,evaluate::evaluate,types::{Action,Rule,PermissionPresetName}};
use serde_json::json;
fn manager(global:serde_json::Value,project:serde_json::Value)->SettingsManager{
    let storage=InMemorySettingsStorage::default();
    storage.with_lock(SettingsScope::Global,&mut |_|Some(global.to_string())).expect("global");
    storage.with_lock(SettingsScope::Project,&mut |_|Some(project.to_string())).expect("project");
    SettingsManager::from_storage(Box::new(storage),true)
}
#[test]
fn default_full_access(){let root=tempfile::tempdir().expect("dir");let (rules,approved)=load_permission_settings(&manager(json!({}),json!({})),&[],(root.path(),"/home/user"),None).expect("settings");assert_eq!(evaluate("bash","rm -rf node_modules",&[&rules]).action,Action::Allow);assert!(approved.is_empty());}
#[test]
fn explicit_ask(){let root=tempfile::tempdir().expect("dir");let (rules,_)=load_permission_settings(&manager(json!({"permissionPreset":"ask"}),json!({})),&[],(root.path(),"/home/user"),None).expect("settings");assert_eq!(evaluate("bash","ls",&[&rules]).action,Action::Ask);}
#[test]
fn cli_overrides(){let root=tempfile::tempdir().expect("dir");let cli=vec![Rule{permission:"bash".into(),pattern:"rm *".into(),action:Action::Deny}];let (rules,_)=load_permission_settings(&manager(json!({"permissionPreset":"read-only"}),json!({})),&cli,(root.path(),"/home/user"),Some(PermissionPresetName::Workspace)).expect("settings");assert_eq!(evaluate("edit","src/index.ts",&[&rules]).action,Action::Allow);assert_eq!(evaluate("bash","rm -rf node_modules",&[&rules]).action,Action::Deny);}
#[test]
fn scope_precedence(){let root=tempfile::tempdir().expect("dir");let cli=vec![Rule{permission:"bash".into(),pattern:"rm *".into(),action:Action::Deny}];let (rules,_)=load_permission_settings(&manager(json!({"permissionPreset":"workspace","permission":{"bash":"deny","edit":"deny"}}),json!({"permissionPreset":"read-only","permission":{"edit":"allow"}})),&cli,(root.path(),"/home/user"),Some(PermissionPresetName::Workspace)).expect("settings");for (permission,pattern,expected) in [("read","README.md",Action::Allow),("edit","src/index.ts",Action::Allow),("bash","git status",Action::Allow),("bash","rm -rf node_modules",Action::Deny),("external_directory","../outside",Action::Ask)]{assert_eq!(evaluate(permission,pattern,&[&rules]).action,expected);}}
#[test]
fn invalid_global(){let root=tempfile::tempdir().expect("dir");assert!(load_permission_settings(&manager(json!({"permissionPreset":"dangerous"}),json!({})),&[],(root.path(),"/home/user"),None).is_err());}
#[test]
fn invalid_project(){let root=tempfile::tempdir().expect("dir");assert!(load_permission_settings(&manager(json!({}),json!({"permissionPreset":"dangerous"})),&[],(root.path(),"/home/user"),None).is_err());}
