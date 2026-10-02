pub fn ensure_valid_cwd() -> std::io::Result<()> {
    if std::env::current_dir().is_err() { let fallback = maho_core::config::home_dir(); std::env::set_current_dir(&fallback)?; eprintln!("the current working directory no longer exists; continuing from {fallback}"); } Ok(())
}
