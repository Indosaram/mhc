use maho_rpc::ownership_safe_lock::{acquire_ownership_safe_lock,LockRetries};
#[tokio::test]
async fn bun_and_native_contenders_share_kernel_sqlite_lock(){
    let temp=tempfile::tempdir().unwrap();let path=temp.path().join("daemon.lock");
    let mut held=acquire_ownership_safe_lock(&path,LockRetries::default()).await.unwrap();
    let script="import {Database} from 'bun:sqlite'; const db=new Database(process.argv[1],{create:true}); try {db.exec('PRAGMA busy_timeout=1; BEGIN EXCLUSIVE;'); process.stdout.write('acquired');db.exec('COMMIT;');} catch(error) {if (!/busy|locked/i.test(String(error))) throw error; process.stdout.write('blocked');} finally {db.close();}";
    let blocked=std::process::Command::new("bun").args(["-e",script]).arg(&path).output().unwrap();assert!(blocked.status.success(),"{}",String::from_utf8_lossy(&blocked.stderr));assert_eq!(blocked.stdout,b"blocked");
    held.release().unwrap();
    let acquired=std::process::Command::new("bun").args(["-e",script]).arg(&path).output().unwrap();assert!(acquired.status.success(),"{}",String::from_utf8_lossy(&acquired.stderr));assert_eq!(acquired.stdout,b"acquired");
}
