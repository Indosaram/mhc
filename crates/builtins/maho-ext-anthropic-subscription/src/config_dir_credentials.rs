use std::{fs,path::{Path,PathBuf}};
use crate::accounts::{AccountSlot,assert_valid_account_name};
const LEGACY:&str="claude-sdk-oauth-accounts";
const CANONICAL:&str="anthropic-subscription-accounts";
fn copy_tree(source:&Path,target:&Path)->std::io::Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry=entry?;let destination=target.join(entry.file_name());let kind=entry.file_type()?;
        if kind.is_dir() {copy_tree(&entry.path(),&destination)?;}
        else if kind.is_symlink() {
            let link=fs::read_link(entry.path())?;
            let link=if link.is_absolute() {link}else {source.join(link)};
            #[cfg(unix)] std::os::unix::fs::symlink(link,&destination)?;
            #[cfg(windows)] {
                if entry.path().is_dir() {std::os::windows::fs::symlink_dir(link,&destination)?;}
                else {std::os::windows::fs::symlink_file(link,&destination)?;}
            }
        } else {fs::copy(entry.path(),destination)?;}
    }
    Ok(())
}
pub fn resolve_accounts_directory(agent_dir:&Path,now_ms:u64)->PathBuf {
    let legacy=agent_dir.join(LEGACY);let target=agent_dir.join(CANONICAL);
    if !legacy.exists() {return target;}
    if target.exists() {let _=fs::rename(&legacy,agent_dir.join(format!("{LEGACY}.{now_ms}.bak")));return target;}
    if fs::rename(&legacy,&target).is_ok() {return target;}
    if copy_tree(&legacy,&target).and_then(|()|fs::remove_dir_all(&legacy)).is_ok() {return target;}
    let _=fs::remove_dir_all(&target);legacy
}
pub fn write_config_dir_credential(agent_dir:&Path,slot:&AccountSlot,access:&str,now_ms:u64)->anyhow::Result<PathBuf> {
    assert_valid_account_name(&slot.name)?;
    let directory=resolve_accounts_directory(agent_dir,now_ms).join(&slot.name);fs::create_dir_all(&directory)?;
    #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;fs::set_permissions(&directory,fs::Permissions::from_mode(0o700))?;}
    let content=serde_json::to_vec(&serde_json::json!({"claudeAiOauth":{"accessToken":access,"refreshToken":slot.refresh,"expiresAt":slot.expires,"scopes":["org:create_api_key","user:profile","user:inference","user:sessions:claude_code","user:mcp_servers","user:file_upload"]}}))?;
    let mut options=fs::OpenOptions::new();options.write(true).create(true).truncate(true);
    #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
    use std::io::Write;options.open(directory.join(".credentials.json"))?.write_all(&content)?;Ok(directory)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn copy_fallback_preserves_symlinks_and_does_not_follow_directory_links() {
        let directory=tempfile::tempdir().expect("directory");let source=directory.path().join("source");let target=directory.path().join("target");fs::create_dir(&source).expect("source");
        fs::write(source.join("state"),b"unchanged").expect("state");
        std::os::unix::fs::symlink("state",source.join("file-link")).expect("file link");
        std::os::unix::fs::symlink(".",source.join("directory-link")).expect("directory link");
        copy_tree(&source,&target).expect("copy");
        assert!(fs::symlink_metadata(target.join("file-link")).expect("metadata").file_type().is_symlink());
        assert_eq!(fs::read_link(target.join("file-link")).expect("link"),source.join("state"));
        assert!(fs::symlink_metadata(target.join("directory-link")).expect("metadata").file_type().is_symlink());
        assert_eq!(fs::read(target.join("state")).expect("state"),b"unchanged");
    }
    #[test]
    fn migration_preserves_bytes_and_canonical_wins_without_merging() {
        let dir=tempfile::tempdir().expect("directory");let legacy=dir.path().join(LEGACY);fs::create_dir(&legacy).expect("legacy");fs::write(legacy.join("state"),b"unchanged").expect("state");let target=resolve_accounts_directory(dir.path(),123);assert_eq!(fs::read(target.join("state")).expect("state"),b"unchanged");assert!(!legacy.exists());fs::create_dir(&legacy).expect("second legacy");fs::write(legacy.join("other"),b"legacy").expect("other");assert_eq!(resolve_accounts_directory(dir.path(),456),target);assert!(!target.join("other").exists());assert!(dir.path().join(format!("{LEGACY}.456.bak/other")).is_file());
    }
    #[test]
    fn credential_shape_and_private_permissions() {
        let dir=tempfile::tempdir().expect("directory");let slot=AccountSlot {name:"work".into(),display_name:None,access:String::new(),refresh:"synthetic-refresh".into(),expires:42.0,source:crate::accounts::AccountSource::Login,blocked_until:None,block_reason:None};let directory=write_config_dir_credential(dir.path(),&slot,"synthetic-access",0).expect("credential");let value:serde_json::Value=serde_json::from_slice(&fs::read(directory.join(".credentials.json")).expect("file")).expect("json");assert_eq!(value["claudeAiOauth"]["accessToken"],"synthetic-access");assert_eq!(value["claudeAiOauth"]["scopes"].as_array().expect("scopes").len(),6);
        #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;assert_eq!(fs::metadata(&directory).expect("dir").permissions().mode()&0o777,0o700);assert_eq!(fs::metadata(directory.join(".credentials.json")).expect("file").permissions().mode()&0o777,0o600);}
    }
}
