use std::sync::Arc;
use crate::{constants::{DIFF_VIEWER_ROW_CAP,GIT_TIMEOUT_MS}, format::sanitize_terminal_text, types::{PanelColor, PanelFuture, PanelGitEntry, PanelGitStatus, PanelRow, PanelUi}};
#[derive(Clone,Debug)] pub struct ExecOutput { pub stdout: String, pub code: i32 }
pub trait PanelExec: Send + Sync { fn exec<'a>(&'a self, command: &'a str, args: Vec<String>, cwd: &'a str, timeout_ms: u64) -> PanelFuture<'a, Result<ExecOutput, String>>; }
pub struct PanelCommandContext { pub mode: Option<String>, pub ui: Option<Arc<dyn PanelUi>> }
pub struct PanelCommandDeps<'a> { pub status: Option<&'a PanelGitStatus>, pub exec: Option<&'a dyn PanelExec>, pub mounted: bool }
fn file_option_label(file: &PanelGitEntry) -> String {
    let delta = if file.added.is_none() && file.removed.is_none() { String::new() } else { format!("  +{}/-{}",file.added.unwrap_or(0),file.removed.unwrap_or(0)) };
    sanitize_terminal_text(&format!("{} {}{delta}",file.xy,file.path))
}
pub async fn run_diff_command(deps: PanelCommandDeps<'_>, ctx: &PanelCommandContext) {
    let Some(ui) = &ctx.ui else { return; };
    if !deps.mounted { ui.notify("The side panel is off in this session.","info"); return; }
    let Some(status) = deps.status.filter(|s|!s.files.is_empty()) else { ui.notify("No changes in the working copy.","info"); return; };
    let Some(exec) = deps.exec else { ui.notify("This host exposes no exec, so git cannot be run.","warning"); return; };
    let options: Vec<_> = status.files.iter().map(file_option_label).collect();
    let Some(choice) = ui.select("Show the diff of",options.clone()).await else { return; };
    let Some(file) = options.iter().position(|o|o == &choice).and_then(|i|status.files.get(i)) else { return; };
    open_file_diff(ui.as_ref(),exec,status,file).await;
}
pub async fn open_file_diff(ui: &dyn PanelUi, exec: &dyn PanelExec, status: &PanelGitStatus, file: &PanelGitEntry) {
    let mut rows = read_git_diff(exec,&status.root,file).await;
    if rows.len() > DIFF_VIEWER_ROW_CAP { let omitted = rows.len() - DIFF_VIEWER_ROW_CAP; rows.truncate(DIFF_VIEWER_ROW_CAP); rows.push(PanelRow::colored(format!("... {omitted} more lines"),PanelColor::Dim)); }
    ui.viewer(&format!("{}  (diff, read-only)",file.path),rows).await;
}
pub fn git_diff_args(file: &PanelGitEntry) -> Vec<String> {
    if file.xy == "??" { return ["--literal-pathspecs","diff","--no-index","--","/dev/null",&file.path].into_iter().map(str::to_owned).collect(); }
    let mut args: Vec<_> = ["--literal-pathspecs","diff","HEAD","-M","--"].into_iter().map(str::to_owned).collect();
    if let Some(from) = &file.from { args.push(from.clone()); } args.push(file.path.clone()); args
}
pub async fn read_git_diff(exec: &dyn PanelExec, root: &str, file: &PanelGitEntry) -> Vec<PanelRow> {
    if file.xy == "??" && file.path.ends_with('/') {
        let args = ["--literal-pathspecs","ls-files","--others","--exclude-standard","-z","--",&file.path].into_iter().map(str::to_owned).collect();
        let Ok(listed) = exec.exec("git",args,root,GIT_TIMEOUT_MS).await else { return diff_error(); };
        if listed.code != 0 { return diff_error(); }
        let paths: Vec<_> = listed.stdout.split('\0').filter(|s|!s.is_empty()).collect();
        let results = futures::future::join_all(paths.iter().take(50).map(|path|exec.exec("git",git_diff_args(&PanelGitEntry { xy:"??".into(),path:(*path).into(),..PanelGitEntry::default() }),root,GIT_TIMEOUT_MS))).await;
        let mut rows = Vec::new();
        for result in results { let Ok(result) = result else { return diff_error(); }; rows.extend(colorize_diff(&result.stdout)); }
        if paths.len() > 50 { rows.push(PanelRow::colored(format!("... {} more files in {}",paths.len()-50,file.path),PanelColor::Muted)); }
        return nonempty_diff(rows);
    }
    let mut result = exec.exec("git",git_diff_args(file),root,GIT_TIMEOUT_MS).await;
    if result.as_ref().is_ok_and(|r|r.code == 128 && r.stdout.is_empty()) && !file.xy.starts_with(' ') {
        let untracked = PanelGitEntry { xy:"??".into(),path:file.path.clone(),..PanelGitEntry::default() };
        result = exec.exec("git",git_diff_args(&untracked),root,GIT_TIMEOUT_MS).await;
    }
    let Ok(result) = result else { return diff_error(); };
    if result.stdout.is_empty() && result.code != 0 && result.code != 1 { return vec![PanelRow::colored(format!("git diff failed (exit {})",result.code),PanelColor::Error)]; }
    nonempty_diff(colorize_diff(&result.stdout))
}
fn diff_error() -> Vec<PanelRow> { vec![PanelRow::colored("git diff could not be run",PanelColor::Error)] }
fn nonempty_diff(rows: Vec<PanelRow>) -> Vec<PanelRow> { if rows.is_empty() { vec![PanelRow::colored("no textual change",PanelColor::Muted)] } else { rows } }
pub fn colorize_diff(output: &str) -> Vec<PanelRow> {
    output.split('\n').filter(|s|!s.is_empty()).map(|line| {
        let color = if line.starts_with("+++") || line.starts_with("---") || ["diff ","index ","new file mode","deleted file mode","old mode","new mode","similarity index","dissimilarity index","rename from","rename to","copy from","copy to","Binary files"].iter().any(|p|line.starts_with(p)) { PanelColor::Muted } else if line.starts_with("@@") { PanelColor::Accent } else if line.starts_with('+') { PanelColor::Success } else if line.starts_with('-') { PanelColor::Error } else { PanelColor::Text };
        PanelRow::colored(line,color)
    }).collect()
}
#[cfg(test)] mod tests {
    use super::*; use crate::types::{WidgetFactory,TerminalHandler,Stop}; use std::sync::Mutex;
    #[derive(Default)] struct Ui { notices:Mutex<Vec<String>>, options:Mutex<Vec<String>>, rows:Mutex<Vec<PanelRow>>, choice:Mutex<Option<usize>> }
    impl PanelUi for Ui {
        fn set_widget(&self,_:&str,_:Option<WidgetFactory>,_:bool) {}
        fn notify(&self,message:&str,_:&str) { self.notices.lock().expect("lock").push(message.into()); }
        fn select<'a>(&'a self,_:&'a str,options:Vec<String>)->PanelFuture<'a,Option<String>> { let choice=self.choice.lock().expect("lock").and_then(|i|options.get(i).cloned());*self.options.lock().expect("lock")=options;Box::pin(async move { choice }) }
        fn viewer<'a>(&'a self,_:&'a str,rows:Vec<PanelRow>)->PanelFuture<'a,()> { *self.rows.lock().expect("lock")=rows;Box::pin(async {}) }
        fn on_terminal_input(&self,_:TerminalHandler)->Option<Stop> { None }
    }
    struct Exec(usize);
    impl PanelExec for Exec { fn exec<'a>(&'a self,_:&'a str,_:Vec<String>,_:&'a str,_:u64)->PanelFuture<'a,Result<ExecOutput,String>> { Box::pin(async move { Ok(ExecOutput { stdout:(0..self.0).map(|i|format!("+line {i}")).collect::<Vec<_>>().join("\n"),code:0 }) }) } }
    fn status()->PanelGitStatus { PanelGitStatus { root:"/repo".into(),files:vec![PanelGitEntry { xy:" M".into(),path:"tracked.txt".into(),added:Some(2),removed:Some(1),..PanelGitEntry::default() },PanelGitEntry { xy:"??".into(),path:"brand-new.txt".into(),..PanelGitEntry::default() }] } }
    #[test] fn select_then_diff_and_cap() { futures::executor::block_on(async {
        let ui=Arc::new(Ui::default());*ui.choice.lock().expect("lock")=Some(0);let ctx=PanelCommandContext { mode:Some("tui".into()),ui:Some(ui.clone()) };let status=status();let exec=Exec(DIFF_VIEWER_ROW_CAP+3);
        run_diff_command(PanelCommandDeps { status:Some(&status),exec:Some(&exec),mounted:true },&ctx).await;
        assert_eq!(*ui.options.lock().expect("lock"),vec![" M tracked.txt  +2/-1","?? brand-new.txt"]);
        let rows=ui.rows.lock().expect("lock");assert_eq!(rows.len(),DIFF_VIEWER_ROW_CAP+1);assert_eq!(rows.last().expect("last").text,"... 3 more lines");
    }); }
    #[test] fn guards_and_dismissal_do_not_open_viewer() { futures::executor::block_on(async {
        let ui=Arc::new(Ui::default());let ctx=PanelCommandContext { mode:None,ui:Some(ui.clone()) };let status=status();let exec=Exec(2);
        for deps in [PanelCommandDeps { status:Some(&status),exec:Some(&exec),mounted:false },PanelCommandDeps { status:None,exec:Some(&exec),mounted:true },PanelCommandDeps { status:Some(&status),exec:None,mounted:true },PanelCommandDeps { status:Some(&status),exec:Some(&exec),mounted:true }] { run_diff_command(deps,&ctx).await; }
        assert_eq!(ui.notices.lock().expect("lock").len(),3);assert!(ui.rows.lock().expect("lock").is_empty());
    }); }
    #[test] fn labels_remove_control_bytes() { let file=PanelGitEntry { xy:"??".into(),path:"evil\x1b]8;;x\x07\u{202e}name.txt".into(),..PanelGitEntry::default() };assert_eq!(file_option_label(&file),"?? evil]8;;xname.txt"); }
}
