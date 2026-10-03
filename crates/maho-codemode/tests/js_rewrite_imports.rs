use maho_codemode::kernels::js::rewrite_imports::{rewrite_imports, DYNAMIC_IMPORT_CALLEE};

#[test]
fn static_bindings_and_side_effects_preserve_import_contract() {
    for (source, expected) in [
        ("import value from \"package-name\";", "const value = (await __senpi_import__(\"package-name\")).default;"),
        ("import { value, other as renamed } from \"package-name\";", "const { value, other: renamed } = await __senpi_import__(\"package-name\");"),
        ("import value, * as namespace from \"package-name\";", "const namespace = await __senpi_import__(\"package-name\"); const value = namespace.default;"),
        ("import value, { other as renamed } from \"package-name\";", "const { default: value, other: renamed } = await __senpi_import__(\"package-name\");"),
        ("import \"polyfill\";", "await __senpi_import__(\"polyfill\");"),
        ("import data from \"./data.json\" with { type: \"json\" };", "const data = (await __senpi_import__(\"./data.json\", { with: { type: \"json\" } })).default;"),
    ] { assert_eq!(rewrite_imports(source), expected); }
}

#[test]
fn dynamic_imports_preserve_arguments_and_nesting() {
    let source = "Promise.all([import('./a.mjs'), import('./b.mjs', { with: { type: 'json' } })]);";
    assert_eq!(rewrite_imports(source), format!("Promise.all([{DYNAMIC_IMPORT_CALLEE}('./a.mjs'), {DYNAMIC_IMPORT_CALLEE}('./b.mjs', {{ with: {{ type: 'json' }} }})]);"));
}

#[test]
fn noncode_import_text_and_unparseable_input_stay_unchanged() {
    for source in ["const quoted = 'import value from x';\nconst templated = `import hidden from x`;\n// import x\n/* import y */", "const value = 40 + 2;\nreturn value;", "import { value from broken syntax 'unterminated"] {
        assert_eq!(rewrite_imports(source), source);
    }
}

#[test]
fn real_imports_do_not_change_template_lookalikes() {
    let source = "import first from 'first';\nconst generated = `import hidden from 'hidden'`;\nimport second from 'second';";
    assert_eq!(rewrite_imports(source), "const first = (await __senpi_import__(\"first\")).default;\nconst generated = `import hidden from 'hidden'`;\nconst second = (await __senpi_import__(\"second\")).default;");
}

#[test]
fn static_sources_decode_javascript_escapes_before_serialization() {
    for source in [r"import 'node:\x70ath';", r"import 'node:\u0070ath';", r"import 'node:\u{70}ath';", "import 'node:pa\\\nth';", r#"import "node:\x70ath";"#] {
        assert_eq!(rewrite_imports(source), "await __senpi_import__(\"node:path\");", "{source}");
    }
    for (source, value) in [(r"import 'a\tb\v\f\0';", "a\tb\u{b}\u{c}\0"), (r"import '\uD83D\uDE00';", "\u{1f600}"), (r"import '\u{1F600}';", "\u{1f600}"), (r"import 'a\\n';", "a\\n"), ("import 'node:pa\\\r\nth';", "node:path")] {
        assert_eq!(rewrite_imports(source), format!("await __senpi_import__({});", serde_json::to_string(value).unwrap()));
    }
}
