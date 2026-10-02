use std::collections::BTreeMap;
#[derive(Clone)]
pub struct CommandOption { pub name: String, pub flag: bool, pub repeatable: bool }
pub struct ParsedCommandInput { pub values: BTreeMap<String, Vec<String>>, pub remaining_args: Vec<String>, pub errors: Vec<String> }
impl ParsedCommandInput { pub fn value(&self, name: &str) -> Option<&str> { self.values.get(name).and_then(|v| v.first()).map(String::as_str) } }
pub fn parse_options(argv: &[String], options: &[CommandOption]) -> ParsedCommandInput {
    let mut parsed = ParsedCommandInput { values: BTreeMap::new(), remaining_args: Vec::new(), errors: Vec::new() }; let mut index = 0;
    while index < argv.len() {
        let argument = &argv[index]; if argument == "--" { parsed.remaining_args.extend_from_slice(&argv[index..]); break; }
        let (name, inline) = argument.split_once('=').map_or((argument.as_str(), None), |(name, value)| (name, Some(value)));
        let Some(option) = options.iter().find(|option| option.name == name) else { parsed.remaining_args.extend_from_slice(&argv[index..]); break; };
        let value = if option.flag { if inline.is_some() { parsed.errors.push(format!("{name} does not take a value")); index += 1; continue; } "".to_owned() } else {
            let candidate = if let Some(value) = inline { Some(value) } else if let Some(value) = argv.get(index + 1).filter(|v| !v.starts_with('-')) { index += 1; Some(value.as_str()) } else { None };
            let Some(value) = candidate.filter(|value| !value.is_empty()) else { parsed.errors.push(format!("{name} requires a value")); index += 1; continue; }; value.to_owned()
        };
        let values = parsed.values.entry(name.to_owned()).or_default(); if !values.is_empty() && !option.repeatable { parsed.errors.push(format!("{name} may only be specified once")); } else { values.push(value); } index += 1;
    } parsed
}
pub fn string_option(name: &str, repeatable: bool) -> CommandOption { CommandOption { name: name.to_owned(), flag: false, repeatable } }
pub fn flag_option(name: &str) -> CommandOption { CommandOption { name: name.to_owned(), flag: true, repeatable: false } }
