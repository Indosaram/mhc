use super::{
    js_installer::EnvironmentError,
    no_links::{assert_link_stays_inside, assert_no_links_below, recorded_install_sources},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

pub struct Revision {
    pub number: u64,
    pub dir: PathBuf,
}
pub fn read_active_revision(base: &Path) -> Result<Option<Revision>, EnvironmentError> {
    let text = match std::fs::read_to_string(base.join("active")) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let name = text.trim();
    let number = name
        .strip_prefix("rev-")
        .filter(|number| !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|number| number.parse::<u64>().ok());
    Ok(number.map(|number| Revision {
        number,
        dir: base.join(name),
    }))
}
pub fn mkdir_private(path: &Path) -> Result<(), EnvironmentError> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    Ok(())
}
fn copy_revision(
    source: &Path,
    target: &Path,
    tree: &Path,
    sources: &std::collections::HashMap<String, PathBuf>,
) -> Result<(), EnvironmentError> {
    let info = std::fs::symlink_metadata(source)?;
    assert_link_stays_inside(source, tree, sources)?;
    if info.file_type().is_symlink() {
        let link = std::fs::read_link(source)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(link, target)?;
        #[cfg(windows)]
        if source.is_dir() {
            std::os::windows::fs::symlink_dir(link, target)?;
        } else {
            std::os::windows::fs::symlink_file(link, target)?;
        }
    } else if info.is_dir() {
        mkdir_private(target)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            copy_revision(
                &entry.path(),
                &target.join(entry.file_name()),
                tree,
                sources,
            )?;
        }
    } else {
        std::fs::copy(source, target)?;
    }
    Ok(())
}
pub struct StagingRevision {
    pub number: u64,
    pub dir: PathBuf,
    pub base: PathBuf,
}
impl Drop for StagingRevision {
    fn drop(&mut self) {
        if let Err(error) = super::js_revision_files::remove_entry(&self.dir) {
            maho_ext_api::diagnostic(&format!("environment staging cleanup failed: {error}"));
        }
    }
}
pub fn stage_next_revision(base: &Path) -> Result<StagingRevision, EnvironmentError> {
    let previous = read_active_revision(base)?;
    let mut highest = previous.as_ref().map_or(0, |revision| revision.number);
    for entry in std::fs::read_dir(base)? {
        let entry = entry?;
        if let Some(number) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_prefix("rev-"))
            .and_then(|number| number.parse::<u64>().ok())
        {
            highest = highest.max(number);
        }
    }
    let number = highest
        .checked_add(1)
        .ok_or_else(|| EnvironmentError::failed("revision number overflow"))?;
    let stage = StagingRevision {
        number,
        dir: base.join(format!(".staging-rev-{number}-{}", uuid::Uuid::new_v4())),
        base: base.into(),
    };
    if let Some(previous) = previous {
        assert_no_links_below(base, &previous.dir)?;
        copy_revision(
            &previous.dir,
            &stage.dir,
            &previous.dir,
            &recorded_install_sources(&previous.dir),
        )?;
    } else {
        mkdir_private(&stage.dir)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stage.dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(stage)
}
async fn rename_retrying(from: &Path, to: &Path) -> Result<(), EnvironmentError> {
    let mut delays = [20, 50, 100, 200, 400, 800].into_iter();
    loop {
        match tokio::fs::rename(from, to).await {
            Ok(()) => return Ok(()),
            Err(error) if cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32 | 33)) => {
                let Some(delay) = delays.next() else {
                    return Err(error.into());
                };
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
}
pub async fn publish_next_revision(
    stage: Arc<StagingRevision>,
    signal: &maho_ext_api::AbortSignal,
) -> Result<Revision, EnvironmentError> {
    if signal.is_aborted() {
        return Err(EnvironmentError::cancelled());
    }
    let name = format!("rev-{}", stage.number);
    let dir = stage.base.join(&name);
    rename_retrying(&stage.dir, &dir).await?;
    let pointer = stage.base.join(format!(".active-{}", uuid::Uuid::new_v4()));
    if signal.is_aborted() {
        return Err(EnvironmentError::cancelled());
    }
    super::js_revision_files::write_private(&pointer, format!("{name}\n").as_bytes())?;
    if let Err(error) = rename_retrying(&pointer, &stage.base.join("active")).await {
        if let Err(cleanup) = super::js_revision_files::remove_entry(&pointer) {
            maho_ext_api::diagnostic(&format!("environment pointer cleanup failed: {cleanup}"));
        }
        return Err(error);
    }
    Ok(Revision {
        number: stage.number,
        dir,
    })
}
