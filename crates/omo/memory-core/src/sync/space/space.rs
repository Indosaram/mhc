//! Sticky space resolution, persisted only in the memory repository's local git config.
use super::{SpaceError, binding::*};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

pub const SYNC_BINDING_CONFIG_KEY: &str = "omo.sync.binding";
pub const SYNC_SPACE_CONFIG_KEY: &str = "omo.sync.space";
pub trait SpaceConfigStore {
    fn get(&self, key: &str) -> Result<Option<String>, SpaceError>;
    fn set(&mut self, key: &str, value: &str) -> Result<(), SpaceError>;
}
#[derive(Default)]
pub struct ResolveSpaceOptions<'a> {
    pub profile: Option<&'a str>,
    pub remotes: &'a [Remote],
    pub random_hex: Option<&'a dyn Fn() -> String>,
}
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SpaceWrites {
    pub binding: Option<String>,
    pub space: Option<String>,
}
#[derive(Debug, PartialEq, Eq)]
pub struct SpaceResolution {
    pub binding_key: BindingKey,
    pub space_id: String,
    pub alias_of_stored_space: bool,
    pub wrote: SpaceWrites,
}
pub fn resolve_space(
    store: &mut impl SpaceConfigStore,
    opts: ResolveSpaceOptions<'_>,
) -> Result<SpaceResolution, SpaceError> {
    let stored_space = store.get(SYNC_SPACE_CONFIG_KEY)?;
    let mut wrote = SpaceWrites::default();
    let profile = opts.profile.unwrap_or("").trim();
    let computed = if !profile.is_empty() && profile != crate::identity::AUTO_AGENT_VALUE {
        named_binding(profile)?
    } else if let Some(repo) = repo_binding(opts.remotes) {
        repo
    } else {
        match store.get(SYNC_BINDING_CONFIG_KEY)? {
            Some(binding) if matches!(parse_binding(&binding), Some((BindingKind::Local, _))) => {
                binding
            }
            _ => {
                let binding = match opts.random_hex {
                    Some(random) => local_binding(&random())?,
                    None => mint_local_binding()?,
                };
                store.set(SYNC_BINDING_CONFIG_KEY, &binding)?;
                wrote.binding = Some(binding.clone());
                binding
            }
        }
    };
    let computed_id = space_id_for_binding(&computed);
    let alias_of_stored_space = stored_space.as_ref().is_some_and(|s| s != &computed_id);
    let space_id = match stored_space {
        Some(id) => id,
        None => {
            store.set(SYNC_SPACE_CONFIG_KEY, &computed_id)?;
            wrote.space = Some(computed_id.clone());
            computed_id
        }
    };
    Ok(SpaceResolution {
        binding_key: computed,
        space_id,
        alias_of_stored_space,
        wrote,
    })
}
pub struct GitSpaceConfig {
    pub repo_dir: PathBuf,
}
impl SpaceConfigStore for GitSpaceConfig {
    fn get(&self, key: &str) -> Result<Option<String>, SpaceError> {
        Ok(
            run_git(&self.repo_dir, &["config", "--local", "--get", key])?
                .map(|s| s.trim().to_owned()),
        )
    }
    fn set(&mut self, key: &str, value: &str) -> Result<(), SpaceError> {
        run_git(&self.repo_dir, &["config", "--local", key, value])?
            .ok_or_else(|| SpaceError::GitWrite(key.into()))?;
        Ok(())
    }
}
pub fn list_remotes(repo_dir: &Path) -> Result<Vec<Remote>, SpaceError> {
    let mut remotes: Vec<Remote> = Vec::new();
    for line in run_git(repo_dir, &["remote", "-v"])?
        .unwrap_or_default()
        .lines()
    {
        let mut parts = line.split_whitespace();
        if let (Some(name), Some(url)) = (parts.next(), parts.next())
            && !remotes.iter().any(|r| r.name == name)
        {
            remotes.push(Remote {
                name: name.into(),
                url: url.into(),
            });
        }
    }
    Ok(remotes)
}
pub fn resolve_space_in_repo(
    repo_dir: &Path,
    profile: Option<&str>,
    random_hex: Option<&dyn Fn() -> String>,
) -> Result<SpaceResolution, SpaceError> {
    let remotes = list_remotes(repo_dir)?;
    resolve_space(
        &mut GitSpaceConfig {
            repo_dir: repo_dir.into(),
        },
        ResolveSpaceOptions {
            profile,
            remotes: &remotes,
            random_hex,
        },
    )
}
fn run_git(dir: &Path, args: &[&str]) -> Result<Option<String>, SpaceError> {
    let output = Command::new("git").arg("-C").arg(dir).args(args).output()?;
    Ok(output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned()))
}
