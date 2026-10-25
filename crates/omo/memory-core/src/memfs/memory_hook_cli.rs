//! Memory hook CLI command entry point.

use std::env;
use std::path::PathBuf;

use crate::memfs::memory_hook::run_memory_hook;

/// Run memory hook validation from command line arguments.
pub fn run_cli() -> i32 {
    let args: Vec<String> = env::args().skip(3).collect();
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    run_memory_hook(&args, &cwd)
}
