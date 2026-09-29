use std::path::Path;

use super::install_script::AST_GREP_BIN_DIR_ENV_KEY;
use super::resolver::SgResolverOptions;
use super::{
    SG_PATH_ENV_KEY, SgCandidate, SgResolutionTier, env_value, runtime_slug, sg_binary_name,
};
use crate::runtime::{node_arch, node_platform};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SgCandidatePlan {
    pub before_path: Vec<SgCandidate>,
    pub after_path: Vec<SgCandidate>,
    pub path_commands: Vec<String>,
}

fn join(dir: &str, parts: &[&str]) -> String {
    parts
        .iter()
        .fold(Path::new(dir).to_path_buf(), |path, part| path.join(part))
        .to_string_lossy()
        .into_owned()
}

fn candidate(tier: SgResolutionTier, path: String) -> SgCandidate {
    SgCandidate { path, tier }
}

fn ast_grep_binary_name(platform: &str) -> &'static str {
    if platform == "win32" {
        "ast-grep.exe"
    } else {
        "ast-grep"
    }
}

fn homebrew_prefixes(platform: &str) -> &'static [&'static str] {
    match platform {
        "darwin" => &["/opt/homebrew/bin", "/usr/local/bin"],
        "linux" => &["/home/linuxbrew/.linuxbrew/bin", "/usr/local/bin"],
        _ => &[],
    }
}

pub fn plan_sg_candidates(options: &SgResolverOptions<'_>) -> SgCandidatePlan {
    let platform = options.platform.unwrap_or(node_platform());
    let arch = options.arch.unwrap_or(node_arch());
    let home_dir = options.home_dir.map_or_else(
        || {
            dirs::home_dir()
                .map(|home| home.to_string_lossy().into_owned())
                .unwrap_or_default()
        },
        str::to_string,
    );
    let binary = sg_binary_name(platform);
    let slug = runtime_slug(platform, arch);
    let names = [ast_grep_binary_name(platform), binary];

    let mut before_path = Vec::new();
    if let Some(over) = env_value(options.env, SG_PATH_ENV_KEY) {
        before_path.push(candidate(SgResolutionTier::EnvOverride, over));
    }
    if let Some(runtime_dir) = options.runtime_dir {
        before_path.push(candidate(
            SgResolutionTier::OmoRuntime,
            join(runtime_dir, &[binary]),
        ));
    }
    if let Some(codex_home) = env_value(options.env, "CODEX_HOME") {
        before_path.push(candidate(
            SgResolutionTier::OmoRuntime,
            join(&codex_home, &["runtime", "ast-grep", &slug, binary]),
        ));
    }
    before_path.push(candidate(
        SgResolutionTier::OmoRuntime,
        join(&home_dir, &[".maho", "runtime", "ast-grep", &slug, binary]),
    ));
    let mut skill_dirs = Vec::new();
    if let Some(cache_dir) = env_value(options.env, AST_GREP_BIN_DIR_ENV_KEY) {
        skill_dirs.push(cache_dir);
    }
    if let Some(package_dir) = options.package_dir {
        skill_dirs.push(join(package_dir, &["bin"]));
    }
    for dir in &skill_dirs {
        before_path.extend(
            names
                .iter()
                .map(|name| candidate(SgResolutionTier::SkillBin, join(dir, &[name]))),
        );
    }
    let after_path = homebrew_prefixes(platform)
        .iter()
        .flat_map(|prefix| {
            names
                .iter()
                .map(move |name| candidate(SgResolutionTier::Homebrew, join(prefix, &[name])))
        })
        .collect();
    SgCandidatePlan {
        before_path,
        after_path,
        path_commands: vec!["ast-grep".to_string(), "sg".to_string()],
    }
}
