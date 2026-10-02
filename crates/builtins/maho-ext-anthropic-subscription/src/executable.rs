use std::path::{Path,PathBuf};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum ExecutableSource {Override,Bundled,Path}
#[derive(Debug,PartialEq,Eq)]
pub struct ExecutableResolution {pub executable:Option<PathBuf>,pub source:Option<ExecutableSource>,pub tried:Vec<String>}
pub fn candidates(platform:&str,arch:&str,prefer_musl:bool)->Vec<String> {
    let extension=if platform=="win32" {".exe"}else {""};let glibc=format!("@anthropic-ai/claude-agent-sdk-{platform}-{arch}/claude{extension}");
    if platform!="linux" {return vec![glibc];}let musl=format!("@anthropic-ai/claude-agent-sdk-linux-{arch}-musl/claude");if prefer_musl {vec![musl,glibc]}else {vec![glibc,musl]}
}
pub fn find_on_posix_path(name:&str,path:Option<&str>)->Option<PathBuf> {path?.split(':').filter(|directory|!directory.is_empty()).map(|directory|Path::new(directory).join(name)).find(|candidate|candidate.is_file())}
pub fn find_on_path(name:&str,platform:&str,path:Option<&str>,pathext:Option<&str>,mut skipped:impl FnMut(&Path))->Option<PathBuf> {
    if platform!="win32" {return find_on_posix_path(name,path);}
    let names:Vec<_>=pathext.unwrap_or(".COM;.EXE;.BAT;.CMD").split(';').filter(|extension|!extension.is_empty()).map(|extension|format!("{name}{}",extension.to_lowercase())).collect();
    let pattern=regex::Regex::new(r#"(?i)"%~?dp0%?\\([^"%]+?\.exe)""#).expect("native cmd shim");
    for directory in path?.split(';').map(|directory|directory.strip_prefix('"').and_then(|directory|directory.strip_suffix('"')).unwrap_or(directory)).filter(|directory|!directory.is_empty()) {
        for name in &names {
            let candidate=Path::new(directory).join(name);if !candidate.is_file() {continue;}
            if !matches!(candidate.extension().and_then(|extension|extension.to_str()),Some("cmd"|"bat")) {return Some(candidate);}
            if let Ok(text)=std::fs::read_to_string(&candidate)
                && let Some(target)=pattern.captures(&text).and_then(|capture|capture.get(1)) {
                let target=Path::new(directory).join(target.as_str().replace('\\',std::path::MAIN_SEPARATOR_STR));
                if target.is_file() {return Some(target);}
            }
            skipped(&candidate);
        }
    }
    None
}
pub struct ResolveInput<'a> {pub platform:&'a str,pub arch:&'a str,pub prefer_musl:bool,pub override_path:Option<&'a Path>,pub path:Option<&'a str>}
pub type VersionLookup<'a>=dyn Fn(&Path)->Option<String>+'a;
pub fn describe(input:ResolveInput<'_>,resolve:impl Fn(&str)->Option<PathBuf>,version:Option<&VersionLookup<'_>>,bundled_version:Option<&str>)->std::io::Result<ExecutableResolution> {
    let mut resolution=ExecutableResolution {executable:None,source:None,tried:Vec::new()};
    if let Some(override_path)=input.override_path.filter(|p|!p.as_os_str().is_empty()) {let absolute=std::path::absolute(override_path)?;resolution.tried.push(absolute.display().to_string());if absolute.is_file() {resolution.executable=Some(absolute);resolution.source=Some(ExecutableSource::Override);return Ok(resolution);}}
    for candidate in candidates(input.platform,input.arch,input.prefer_musl) {
        let Some(path)=resolve(&candidate) else {resolution.tried.push(candidate);continue;};let bundled=std::path::absolute(path)?;resolution.tried.push(bundled.display().to_string());if !bundled.is_file() {continue;}
        if let (Some(version),Some(on_path))=(version,find_on_path("claude",input.platform,input.path,std::env::var("PATHEXT").ok().as_deref(),|_|{})) {let absolute=std::path::absolute(on_path)?;
            if absolute!=bundled {let path_version=version(&absolute);let baseline=bundled_version.map(str::to_owned).or_else(||version(&bundled));if crate::executable_version::is_newer(path_version.as_deref(),baseline.as_deref()) {resolution.tried.push(absolute.display().to_string());resolution.executable=Some(absolute);resolution.source=Some(ExecutableSource::Path);return Ok(resolution);}}
        }
        resolution.executable=Some(bundled);resolution.source=Some(ExecutableSource::Bundled);return Ok(resolution);
    }
    if let Some(path)=find_on_path("claude",input.platform,input.path,std::env::var("PATHEXT").ok().as_deref(),|path|resolution.tried.push(format!("{} (batch file wrapping no native binary)",path.display()))) {let absolute=std::path::absolute(path)?;resolution.tried.push(absolute.display().to_string());resolution.executable=Some(absolute);resolution.source=Some(ExecutableSource::Path);}else {resolution.tried.push(if input.path.is_some_and(|p|!p.is_empty()) {"claude on PATH"}else {"claude on PATH (PATH is unset)"}.into());}Ok(resolution)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_path_resolves_native_shims_and_skips_shell_only_batches() {
        let directory=tempfile::tempdir().expect("directory");let native=directory.path().join("node_modules/bin/claude.exe");std::fs::create_dir_all(native.parent().expect("parent")).expect("directories");std::fs::write(&native,"").expect("native");
        std::fs::write(directory.path().join("claude.bat"),"echo shell only").expect("batch");
        std::fs::write(directory.path().join("claude.cmd"),r#""%dp0%\node_modules\bin\claude.exe" %*"#).expect("shim");
        let path=format!("\"{}\"",directory.path().display());let mut skipped=Vec::new();
        assert_eq!(find_on_path("claude","win32",Some(&path),Some(".BAT;.CMD"),|path|skipped.push(path.to_owned())),Some(native));
        assert_eq!(skipped,vec![directory.path().join("claude.bat")]);
    }
    #[test]
    fn override_wins_and_newer_path_supersedes_bundled() {
        let directory=tempfile::tempdir().expect("directory");let bundled=directory.path().join("bundled");let on_path=directory.path().join("claude");let explicit=directory.path().join("explicit");for path in [&bundled,&on_path,&explicit] {std::fs::write(path,"").expect("file");}let path=directory.path().to_str().expect("path");
        let input=|override_path|ResolveInput {platform:"linux",arch:"x64",prefer_musl:false,override_path,path:Some(path)};let version=|candidate:&Path|Some(if candidate==on_path {"2.1.300"}else {"2.1.280"}.into());
        assert_eq!(describe(input(Some(&explicit)),|_|Some(bundled.clone()),Some(&version),None).expect("override").source,Some(ExecutableSource::Override));assert_eq!(describe(input(None),|_|Some(bundled.clone()),Some(&version),None).expect("path").executable,Some(on_path));assert_eq!(describe(input(None),|_|Some(bundled.clone()),None,None).expect("bundled").source,Some(ExecutableSource::Bundled));
    }
    #[test]
    fn directories_are_rejected_and_candidate_order_is_platform_specific() {let directory=tempfile::tempdir().expect("directory");assert!(find_on_posix_path("missing",directory.path().to_str()).is_none());assert!(candidates("linux","arm64",true)[0].contains("musl"));assert!(candidates("win32","x64",false)[0].ends_with("claude.exe"));}
}
