mod archive;
mod download;
pub mod install;
mod layout;
mod lease;
mod manifest;
mod processes;
pub mod release;
mod transaction;
#[cfg(windows)]
mod windows_helper;

pub async fn update() -> Result<(), String> {
    let latest = release::latest().await?;
    if release::compare_versions(&latest.version, release::current_version())?
        != std::cmp::Ordering::Greater
    {
        maho_core::output_guard::maho_write_stdout(&format!("mhc {} is up to date.\n", release::current_version()));
        return Ok(());
    }
    match install::install(&latest).await? {
        install::InstallOutcome::Installed => {
            maho_core::output_guard::maho_write_stdout(&format!("Updated mhc to {}.\n", latest.version));
        }
    }
    Ok(())
}
