use std::path::Path;

const HARNESS_PACKAGES: &[&str] = &[
    "maho-core", "maho-cli", "maho-rpc", "maho-ext-", "maho-omo-", "maho-senpi-task",
];
const HARNESS_MODULES: &[&str] = &[
    "maho_core", "maho_cli", "maho_rpc", "maho_ext_", "maho_omo_", "senpi_task",
];

#[test]
fn manifest_does_not_depend_on_a_harness() {
    let manifest=std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).expect("memory manifest");
    let mut dependencies=false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            dependencies=line.contains("dependencies");
        } else if dependencies && let Some((name,_))=line.split_once('=') {
            let name=name.trim().trim_matches('"');
            for forbidden in HARNESS_PACKAGES {assert!(!name.starts_with(forbidden),"harness dependency {name}");}
        }
    }
}

#[test]
fn sources_do_not_import_a_harness() {
    fn scan(dir:&Path,files:&mut usize) {
        for entry in std::fs::read_dir(dir).expect("memory source directory") {
            let path=entry.expect("source entry").path();
            if path.is_dir() {scan(&path,files);} else if path.extension().is_some_and(|extension|extension=="rs") {
                *files+=1;
                let source=std::fs::read_to_string(&path).expect("memory source");
                for forbidden in HARNESS_MODULES {
                    for prefix in ["use ","extern crate "] {
                        assert!(!source.contains(&format!("{prefix}{forbidden}")),"harness import in {}",path.display());
                    }
                }
            }
        }
    }
    let mut files=0;
    scan(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),&mut files);
    assert!(files>0);
}
