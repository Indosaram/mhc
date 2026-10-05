use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

pub const AGENT_DIR_ENV_NAMES: [&str; 3] = [
    "OMO_CODING_AGENT_DIR", "SENPI_CODING_AGENT_DIR", "PI_CODING_AGENT_DIR",
];
pub const AGENT_HOME_SENTINEL: &str = "settings.json";

pub fn resolve_agent_home(
    env: &BTreeMap<String, String>, home_dir: &Path, cwd: &Path,
    exists: impl Fn(&Path) -> bool,
) -> PathBuf {
    for name in AGENT_DIR_ENV_NAMES {
        if let Some(configured) = env.get(name).map(|s| s.trim()).filter(|s| !s.is_empty()) {
            let path = cwd.join(configured);
            let mut resolved = PathBuf::new();
            for component in path.components() {
                match component {
                    Component::CurDir => {},
                    Component::ParentDir => { resolved.pop(); },
                    other => resolved.push(other),
                }
            }
            return resolved;
        }
    }
    let branded = home_dir.join(".maho");
    let canonical = branded.join("agent");
    if exists(&canonical.join(AGENT_HOME_SENTINEL)) { return canonical; }
    if exists(&branded.join(AGENT_HOME_SENTINEL)) { return branded; }
    home_dir.join(".senpi/agent")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn resolve(env: &[(&str, &str)], exists: impl Fn(&Path) -> bool) -> PathBuf {
        resolve_agent_home(&env.iter().map(|(k,v)| (k.to_string(),v.to_string())).collect(), Path::new("/home/tester"), Path::new("/cwd"), exists)
    }
    #[test] fn branded_name_wins() {
        assert_eq!(resolve(&[("OMO_CODING_AGENT_DIR","/explicit/omo"),("SENPI_CODING_AGENT_DIR","/explicit/senpi"),("PI_CODING_AGENT_DIR","/explicit/pi")], |_| false), Path::new("/explicit/omo"));
    }
    #[test] fn legacy_names_work() {
        for key in &AGENT_DIR_ENV_NAMES[1..] { assert_eq!(resolve(&[(key,"/explicit/legacy")], |_| false), Path::new("/explicit/legacy")); }
    }
    #[test] fn blank_value_ignored() { assert_eq!(resolve(&[("OMO_CODING_AGENT_DIR","   ")], |_| false), Path::new("/home/tester/.senpi/agent")); }
    #[test] fn flat_branded_home_detected() { assert_eq!(resolve(&[], |p| p == Path::new("/home/tester/.maho/settings.json")), Path::new("/home/tester/.maho")); }
    #[test] fn legacy_directory_default() { assert_eq!(resolve(&[], |_| false), Path::new("/home/tester/.senpi/agent")); }
    #[test] fn canonical_wins_over_flat() { assert_eq!(resolve(&[], |p| p == Path::new("/home/tester/.maho/settings.json") || p == Path::new("/home/tester/.maho/agent/settings.json")), Path::new("/home/tester/.maho/agent")); }
    #[test] fn relative_override_normalized() { assert_eq!(resolve(&[("OMO_CODING_AGENT_DIR"," ../chosen/./agent ")], |_| false), Path::new("/chosen/agent")); }
}
