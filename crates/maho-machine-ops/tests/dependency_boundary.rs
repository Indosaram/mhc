use std::path::Path;

#[test]
fn maho_machine_ops_declares_no_engine_dependencies() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let cargo_toml =
        std::fs::read_to_string(manifest_dir.join("Cargo.toml")).expect("read Cargo.toml");
    let forbidden = [
        "maho-core",
        "maho-agent",
        "maho-ai",
        "maho-tools",
        "maho-interactive",
    ];
    for crate_name in forbidden {
        assert!(
            !cargo_toml.contains(crate_name),
            "maho-machine-ops Cargo.toml must remain engine-free and never depend on {crate_name}"
        );
    }

    let src_dir = manifest_dir.join("src");
    let mut stack = vec![src_dir];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).expect("read src dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let source = std::fs::read_to_string(&path).expect("read source");
                for crate_name in forbidden {
                    let ident = crate_name.replace('-', "_");
                    assert!(
                        !source.contains(&ident),
                        "{} must not import or reference engine crate {}",
                        path.display(),
                        ident
                    );
                }
            }
        }
    }
}
