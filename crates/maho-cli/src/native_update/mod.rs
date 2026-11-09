mod archive;
mod download;
pub mod install;
mod layout;
mod lease;
mod manifest;
mod pointer;
pub mod release;
mod retention;
pub mod signature;

pub async fn update(allow_unsigned: bool) -> Result<(), String> {
    let current = release::installed_version()?;
    let latest = release::latest().await?;
    if release::compare_versions(&latest.version, &current)? != std::cmp::Ordering::Greater {
        maho_core::output_guard::maho_write_stdout(&format!("mhc {current} is up to date.\n"));
        return Ok(());
    }
    let installed = install::install(&latest, allow_unsigned).await?;
    for warning in &installed.warnings {
        maho_core::output_guard::maho_write_stderr(&format!("warning: {warning}\n"));
    }
    maho_core::output_guard::maho_write_stdout(&format!(
        "Installed mhc {}. New sessions will use it; running sessions keep their current version.\n",
        installed.version
    ));
    Ok(())
}
