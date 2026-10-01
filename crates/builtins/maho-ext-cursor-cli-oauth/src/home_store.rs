use crate::accounts::{CursorCliAccountSlot,assert_valid_account_name};
use std::{future::Future, path::{Path,PathBuf}, os::unix::fs::PermissionsExt};
use serde_json::{Value,json};

pub struct AccountHomeContext { pub home: PathBuf, pub auth_path: PathBuf }
pub struct AccountHomeRunResult<T> { pub result: T, pub slot: CursorCliAccountSlot, pub home: PathBuf }

pub async fn run_in_account_home<T,F,Fut>(agent_dir: &Path, slot: &CursorCliAccountSlot, run: F, mut log: impl FnMut(String)) -> anyhow::Result<AccountHomeRunResult<T>>
where F: FnOnce(AccountHomeContext) -> Fut, Fut: Future<Output=anyhow::Result<T>> {
    assert_valid_account_name(&slot.name)?;
    let root = if agent_dir.is_absolute() { agent_dir.to_path_buf() } else { std::env::current_dir()?.join(agent_dir) };
    let extension = root.join("cursor-cli-oauth"); let accounts = extension.join("accounts");
    let account = accounts.join(&slot.name); let home = account.join("home"); let cursor = home.join(".cursor");
    for directory in [&extension,&accounts,&account,&home,&cursor] {
        std::fs::create_dir_all(directory)?; std::fs::set_permissions(directory,std::fs::Permissions::from_mode(0o700))?;
    }
    let auth_path = cursor.join("auth.json");
    let credential = json!({"accessToken":slot.access,"refreshToken":slot.refresh,"apiKey":null,"bedrockCredentials":null});
    use std::os::unix::fs::OpenOptionsExt;
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&auth_path)?;
    file.write_all(serde_json::to_string(&credential)?.as_bytes())?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?; drop(file);
    log(format!("cursor_cli_oauth_credential_written accessBytes={} refreshBytes={}",slot.access.len(),slot.refresh.len()));
    let result = run(AccountHomeContext { home:home.clone(),auth_path:auth_path.clone() }).await?;
    let content = std::fs::read_to_string(&auth_path)?;
    let credential:Value = serde_json::from_str(&content).map_err(|_| anyhow::anyhow!("Invalid Cursor credential: auth.json is not valid JSON"))?;
    let Some(access) = credential["accessToken"].as_str().filter(|v| !v.is_empty()) else {
        anyhow::bail!("Invalid Cursor credential: auth.json does not match the file-store contract");
    };
    let Some(refresh) = credential["refreshToken"].as_str().filter(|v| !v.is_empty()) else {
        anyhow::bail!("Invalid Cursor credential: auth.json does not match the file-store contract");
    };
    if credential.get("apiKey") != Some(&Value::Null) || credential.get("bedrockCredentials") != Some(&Value::Null) {
        anyhow::bail!("Invalid Cursor credential: auth.json does not match the file-store contract");
    }
    log(format!("cursor_cli_oauth_credential_read accessBytes={} refreshBytes={} rotated={}",access.len(),refresh.len(),refresh != slot.refresh));
    let mut updated = slot.clone(); updated.refresh = refresh.into();
    Ok(AccountHomeRunResult { result,slot:updated,home })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::AccountSource;
    fn slot() -> CursorCliAccountSlot {
        CursorCliAccountSlot { name:"work".into(),display_name:None,access:"access-token-secret".into(),refresh:"refresh-token-secret".into(),
            expires:4102444800000.0,source:AccountSource::Login,blocked_until:None,block_reason:None }
    }
    #[tokio::test]
    async fn private_tree_and_credentials() {
        let root = tempfile::tempdir().unwrap(); let account = slot();
        let result = run_in_account_home(root.path(),&account,|context| async move {
            assert_eq!(std::fs::metadata(&context.auth_path)?.permissions().mode() & 0o777,0o600);
            assert_eq!(std::fs::metadata(&context.home)?.permissions().mode() & 0o777,0o700);
            let credential:Value = serde_json::from_str(&std::fs::read_to_string(context.auth_path)?)?;
            assert_eq!(credential["apiKey"],Value::Null); assert_eq!(credential["bedrockCredentials"],Value::Null);
            Ok("spawned")
        }, |_| {}).await.unwrap();
        assert_eq!(result.result,"spawned"); assert_eq!(result.slot,account);
    }
    #[tokio::test]
    async fn invalid_names_create_nothing() {
        for name in ["../escape","..","nested/slot","-leading"] {
            let root = tempfile::tempdir().unwrap(); let mut account = slot(); account.name = name.into();
            assert!(run_in_account_home::<(),_,_>(root.path(),&account,|_| async { panic!("must not run") },|_| {}).await.is_err());
            assert!(!root.path().join("cursor-cli-oauth").exists());
        }
    }
    #[tokio::test]
    async fn durable_home_reapplies_and_reads_rotation() {
        let root = tempfile::tempdir().unwrap();
        let first = run_in_account_home(root.path(),&slot(),|context| async move {
            std::fs::write(context.home.join(".cursor/cli-config.json"),"retained")?;
            std::fs::write(context.auth_path,json!({"accessToken":"access-token-secret","refreshToken":"rotated-one","apiKey":null,"bedrockCredentials":null}).to_string())?;
            Ok(())
        },|_| {}).await.unwrap();
        assert_eq!(first.slot.refresh,"rotated-one");
        let second = run_in_account_home(root.path(),&first.slot,|context| async move {
            assert_eq!(std::fs::read_to_string(context.home.join(".cursor/cli-config.json"))?,"retained");
            let credential:Value = serde_json::from_str(&std::fs::read_to_string(&context.auth_path)?)?;
            assert_eq!(credential["refreshToken"],"rotated-one");
            std::fs::write(context.auth_path,json!({"accessToken":"changed-access","refreshToken":"rotated-two","apiKey":null,"bedrockCredentials":null}).to_string())?;
            Ok(())
        },|_| {}).await.unwrap(); assert_eq!(second.slot.refresh,"rotated-two"); assert_eq!(second.slot.access,"access-token-secret");
    }
    #[tokio::test]
    async fn malformed_cli_credential_rejected() {
        let root = tempfile::tempdir().unwrap();
        assert!(run_in_account_home(root.path(),&slot(),|context| async move {
            std::fs::write(context.auth_path,"{\"refreshToken\":{\"token\":\"misleading\"}}")?; Ok(())
        },|_| {}).await.is_err());
    }
    #[tokio::test]
    async fn logs_lengths_not_material() {
        let root = tempfile::tempdir().unwrap(); let mut lines = Vec::new(); let account = slot();
        run_in_account_home(root.path(),&account,|_| async { Ok(()) },|line| lines.push(line)).await.unwrap();
        let output = lines.join("\n"); assert!(output.contains(&account.access.len().to_string()));
        assert!(!output.contains(&account.access)); assert!(!output.contains(&account.refresh));
    }
}
