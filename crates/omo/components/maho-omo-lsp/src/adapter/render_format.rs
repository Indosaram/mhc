fn percent_decode(s:&str)->String {let bytes=s.as_bytes();let mut out=Vec::with_capacity(bytes.len());let mut i=0;while i<bytes.len() {if bytes[i]==b'%'&&i+2<bytes.len()&&let Ok(b)=u8::from_str_radix(&s[i+1..i+3],16) {out.push(b);i+=3;continue;}out.push(bytes[i]);i+=1;}String::from_utf8_lossy(&out).into_owned()}
pub fn uri_to_path(uri:&str)->Result<String,String> {let uri=url::Url::parse(uri).map_err(|e|e.to_string())?;if let Ok(p)=uri.to_file_path() {return Ok(p.to_string_lossy().into_owned());}
    if uri.scheme()=="file" {return Ok(percent_decode(uri.path()));}Err("Invalid file URL".into())}
pub fn shorten(value:&str,max:usize)->String {let units:Vec<_>=value.encode_utf16().collect();if units.len()<=max {value.into()} else {format!("{}...",String::from_utf16_lossy(&units[..max.saturating_sub(1)]))}}
#[cfg(test)]
mod tests {use super::*;#[test] fn uri_decodes_spaces() {assert_eq!(uri_to_path("file:///tmp/a%20b.ts").unwrap(),"/tmp/a b.ts");assert!(uri_to_path("https://example.org/x").is_err());}#[test] fn shorten_uses_upstream_budget() {assert_eq!(shorten("abcdef",4),"abc...");assert_eq!(shorten("abc",4),"abc");}}
