use std::{collections::BTreeMap, sync::OnceLock};
pub fn case_fold(text: &str) -> String {
    static TABLE: OnceLock<BTreeMap<char, String>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut table = BTreeMap::new();
        for line in include_str!("CaseFolding.txt").lines() {
            let mut fields = line.split(';').map(str::trim);
            let (Some(code), Some(status), Some(mapping)) =
                (fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            if !matches!(status, "C" | "F") {
                continue;
            }
            let Some(code) = u32::from_str_radix(code, 16).ok().and_then(char::from_u32) else {
                continue;
            };
            let mapped: Option<String> = mapping
                .split_whitespace()
                .map(|s| u32::from_str_radix(s, 16).ok().and_then(char::from_u32))
                .collect();
            if let Some(mapped) = mapped
                && (status == "F" || !table.contains_key(&code))
            {
                table.insert(code, mapped);
            }
        }
        table
    });
    let mut out = String::new();
    for c in text.chars() {
        if let Some(mapped) = table.get(&c) {
            out.push_str(mapped);
        } else {
            out.push(c);
        }
    }
    out
}
