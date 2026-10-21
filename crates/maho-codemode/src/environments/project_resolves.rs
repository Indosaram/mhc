use std::path::Path;
pub fn legacy_file(path: &Path) -> bool {
    path.is_file()
        || ["js", "mjs", "cjs"].iter().any(|extension| {
            std::path::PathBuf::from(format!("{}.{extension}", path.display())).is_file()
        })
        || path.join("index.js").is_file()
}
pub fn project_resolves(cwd: &Path, name: &str) -> bool {
    for directory in cwd.ancestors() {
        let package = directory.join("node_modules").join(name);
        if !package.is_dir() {
            continue;
        }
        let manifest = std::fs::read(package.join("package.json"))
            .ok()
            .and_then(|text| serde_json::from_slice::<serde_json::Value>(&text).ok());
        return match manifest {
            Some(manifest) if manifest.is_null() => true,
            Some(manifest) if manifest.is_object() => {
                !manifest["exports"].is_null()
                    || legacy_file(
                        &package.join(
                            manifest["module"]
                                .as_str()
                                .or_else(|| manifest["main"].as_str())
                                .unwrap_or("index.js"),
                        ),
                    )
            }
            _ => legacy_file(&package.join("index.js")),
        };
    }
    false
}
