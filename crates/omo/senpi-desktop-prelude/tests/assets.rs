use senpi_desktop_prelude::ComputerPreludeAssets;

const ASSET_JS: &str = include_str!("../src/prelude.js");
const ASSET_PY: &str = include_str!("../src/prelude.py");
const ASSET_DECL: &str = include_str!("../declarations.d.ts");
const ASSET_DOC: &str = include_str!("../docs/computer.md");
const ASSET_SAFETY: &str = include_str!("../docs/computer-safety.md");

#[test]
fn keeps_the_committed_assets_json_in_sync_with_the_asset_files() {
    assert_eq!(ComputerPreludeAssets::javascript(), ASSET_JS);
    assert_eq!(ComputerPreludeAssets::python(), ASSET_PY);
    assert_eq!(ComputerPreludeAssets::declarations(), ASSET_DECL);
    assert_eq!(ComputerPreludeAssets::documentation(), ASSET_DOC);
    assert_eq!(ComputerPreludeAssets::safety(), ASSET_SAFETY);
}

#[test]
fn defines_every_declared_export_in_the_javascript_kernel() {
    let js = ComputerPreludeAssets::javascript();
    for export in ComputerPreludeAssets::exports() {
        assert!(js.contains(&format!("globalThis.{} =", export)) || js.contains(&format!("{}:", export)));
    }
}

#[test]
fn defines_every_declared_export_in_the_python_kernel() {
    let py = ComputerPreludeAssets::python();
    for export in ComputerPreludeAssets::exports() {
        assert!(py.contains(&format!("{} =", export)) || py.contains(&format!("class {}", export)));
    }
}

#[test]
fn documents_every_facade_helper_as_a_call_in_the_model_facing_docs() {
    let doc = ComputerPreludeAssets::documentation();
    for method in ComputerPreludeAssets::method_allowlist() {
        let pattern = format!("{}(", method);
        assert!(doc.contains(&pattern), "Documentation missing pattern: {}", pattern);
    }
}

#[test]
fn keeps_the_documentation_free_of_code_fences_since_the_eval_prompt_renders_it_inside_one() {
    assert!(!ComputerPreludeAssets::documentation().contains("```"));
}
