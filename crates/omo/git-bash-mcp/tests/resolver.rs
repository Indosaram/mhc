//! The two pinned `git-bash-resolver.test.ts` cases that `maho-utils` does not already port
//! (`crates/omo/utils/tests/processes.rs` covers the other five). The resolver itself is reused
//! from `maho-utils`; these tests drive it through this crate's re-export.

use std::collections::HashMap;

use git_bash_mcp::GitBashResolution;
use git_bash_mcp::GitBashResolverInput;
use git_bash_mcp::GitBashSource;
use git_bash_mcp::resolve_git_bash;
use pretty_assertions::assert_eq;

const PROGRAM_FILES_GIT_BASH: &str = "C:\\Program Files\\Git\\bin\\bash.exe";
const PROGRAM_FILES_X86_GIT_BASH: &str = "C:\\Program Files (x86)\\Git\\bin\\bash.exe";

fn resolve(
    env: &[(&str, &str)],
    exists: &dyn Fn(&str) -> bool,
    listed: &[&str],
) -> GitBashResolution {
    let env: HashMap<String, String> = env
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    let listed: Vec<String> = listed.iter().map(|path| (*path).to_owned()).collect();
    resolve_git_bash(&GitBashResolverInput {
        platform: "win32",
        env: &env,
        exists,
        where_bash: &|| listed.clone(),
    })
}

fn probes(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|path| (*path).to_owned()).collect()
}

#[test]
fn only_the_system32_launcher_on_path_fails_resolution() {
    let system32_bash = "C:\\Windows\\System32\\bash.exe";
    let result = resolve(&[], &|path| path == system32_bash, &[system32_bash]);

    let GitBashResolution::Missing { checked_paths, .. } = result else {
        panic!("expected a missing resolution");
    };
    assert_eq!(
        checked_paths,
        probes(&[
            PROGRAM_FILES_GIT_BASH,
            PROGRAM_FILES_X86_GIT_BASH,
            system32_bash,
        ])
    );
}

#[test]
fn mixed_case_system32_launcher_is_skipped_case_insensitively() {
    let system32_bash = "c:\\WINDOWS\\System32\\BASH.EXE";
    let git_bash = "E:\\Git\\bin\\bash.exe";
    let result = resolve(
        &[],
        &|path| path == system32_bash || path == git_bash,
        &[system32_bash, git_bash],
    );

    assert_eq!(
        result,
        GitBashResolution::Found {
            path: Some(git_bash.to_owned()),
            source: GitBashSource::Path,
            checked_paths: probes(&[
                PROGRAM_FILES_GIT_BASH,
                PROGRAM_FILES_X86_GIT_BASH,
                system32_bash,
                git_bash,
            ]),
        }
    );
}
