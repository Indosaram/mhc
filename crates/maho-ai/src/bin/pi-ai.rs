//! `pi-ai` binary (senpi packages/ai package.json `bin`): runs `maho_ai::cli::main`.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(anyhow::Error::from)
        .and_then(|runtime| runtime.block_on(maho_ai::cli::main(&args)));
    if let Err(error) = result {
        eprintln!("Error: {error}");
        std::process::exit(1);
    }
}
