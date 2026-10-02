pub fn uri_to_path(uri:&str)->Result<String,String> {let uri=url::Url::parse(uri).map_err(|e|e.to_string())?;uri.to_file_path().map(|p|p.to_string_lossy().into_owned()).map_err(|()|"Invalid file URL".into())}
pub fn shorten(value:&str,max:usize)->String {let units:Vec<_>=value.encode_utf16().collect();if units.len()<=max {value.into()} else {format!("{}...",String::from_utf16_lossy(&units[..max.saturating_sub(1)]))}}
#[cfg(test)]
mod tests {use super::*;#[test] fn uri_decodes_spaces() {assert_eq!(uri_to_path("file:///tmp/a%20b.ts").unwrap(),"/tmp/a b.ts");assert!(uri_to_path("https://example.org/x").is_err());}#[test] fn shorten_uses_upstream_budget() {assert_eq!(shorten("abcdef",4),"abc...");assert_eq!(shorten("abc",4),"abc");}}
