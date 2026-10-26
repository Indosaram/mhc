//! `mhc doctor` in the MIT profile (`--no-default-features`). The full diagnostics resolve the
//! omo configuration and delegate/category models through crates that are only linked with the
//! `omo` feature, so this build reports that the command is unavailable.

pub const UNAVAILABLE: &str =
    "mhc doctor is not available in this build (MIT profile, built without the `omo` feature).";

pub fn run_doctor(args: &[String]) -> Result<i32, String> {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("Usage: mhc doctor\n\n{UNAVAILABLE}");
        return Ok(0);
    }
    maho_core::output_guard::maho_write_stderr(&format!("{UNAVAILABLE}\n"));
    Ok(1)
}
