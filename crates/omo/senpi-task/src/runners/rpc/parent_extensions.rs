//! `runners/rpc/parent-extensions.ts`: forward the parent's `-e`/`--extension` entries to a
//! detached rpc child, which cannot inherit the parent's in-memory extension registry.

pub fn parse_extension_entries(argv: &[String]) -> Vec<String> {
    let mut entries = Vec::new();
    let mut index = 0;
    while index < argv.len() {
        let flag = argv[index].as_str();
        if (flag == "-e" || flag == "--extension")
            && let Some(value) = argv.get(index + 1).filter(|value| !value.is_empty())
        {
            entries.push(value.clone());
            index += 1;
        }
        index += 1;
    }
    entries
}
