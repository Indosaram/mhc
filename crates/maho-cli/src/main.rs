fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [flag] if flag == "--version" => println!("mhc {}", env!("CARGO_PKG_VERSION")),
        _ => {
            eprintln!("usage: mhc --version");
            std::process::exit(2);
        }
    }
}
