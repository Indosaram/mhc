//! Order-preserving string list unions.

use std::collections::HashSet;

pub fn merge_unique_strings<S: AsRef<str>>(
    base: Option<&[S]>,
    override_value: Option<&[S]>,
) -> Vec<String> {
    let mut seen = HashSet::new();
    chain(base, override_value)
        .filter(|value| seen.insert(value.to_string()))
        .map(str::to_string)
        .collect()
}

pub fn merge_unique_strings_case_insensitive<S: AsRef<str>>(
    base: Option<&[S]>,
    override_value: Option<&[S]>,
) -> Vec<String> {
    let mut seen = HashSet::new();
    chain(base, override_value)
        .filter(|value| seen.insert(value.to_lowercase()))
        .map(str::to_string)
        .collect()
}

fn chain<'a, S: AsRef<str>>(
    base: Option<&'a [S]>,
    over: Option<&'a [S]>,
) -> impl Iterator<Item = &'a str> {
    base.unwrap_or_default()
        .iter()
        .chain(over.unwrap_or_default().iter())
        .map(AsRef::as_ref)
}
