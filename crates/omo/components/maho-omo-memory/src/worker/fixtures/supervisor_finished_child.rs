use std::path::Path;
pub fn run(run_dir: &Path) -> std::io::Result<()> {
    std::fs::write(run_dir.join("child-finished.json"), "{\"finished\":true}\n")
}
