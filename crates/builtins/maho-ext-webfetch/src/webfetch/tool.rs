use super::fetcher::WebfetchFormat;
pub const DEFAULT_OUTPUT_MAX_BYTES:usize=50*1024;
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct WebfetchOutputCap { pub text:String,pub notice:Option<String>,pub truncated:bool,pub output_bytes:usize,pub total_bytes:usize }
pub fn parse_webfetch_format(value:Option<&str>)->WebfetchFormat { match value { Some("text")=>WebfetchFormat::Text,Some("html")=>WebfetchFormat::Html,_=>WebfetchFormat::Markdown } }
fn format_byte_size(bytes:usize)->String {
    if bytes<1024 { format!("{bytes}B") } else if bytes<1024*1024 { format!("{:.1}KB",bytes as f64/1024.0) } else { format!("{:.1}MB",bytes as f64/(1024.0*1024.0)) }
}
fn take_head_bytes(text:&str,max_bytes:usize)->String {
    let mut lines=vec![]; let mut used=0;
    for line in text.split('\n') { let bytes=line.len()+usize::from(!lines.is_empty()); if used+bytes>max_bytes { break; } lines.push(line); used+=bytes; }
    if !lines.is_empty() { return lines.join("\n"); }
    let mut end=max_bytes.min(text.len()); while !text.is_char_boundary(end) { end-=1; }
    let prefix=&text[..end];
    if end==max_bytes && prefix.ends_with('\u{fffd}') { prefix.strip_suffix('\u{fffd}').unwrap_or(prefix).into() } else { prefix.into() }
}
pub fn cap_webfetch_output(text:&str)->WebfetchOutputCap {
    let total_bytes=text.len();
    if total_bytes<=DEFAULT_OUTPUT_MAX_BYTES { return WebfetchOutputCap{text:text.into(),notice:None,truncated:false,output_bytes:total_bytes,total_bytes}; }
    let head=take_head_bytes(text,DEFAULT_OUTPUT_MAX_BYTES); let output_bytes=head.len();
    let notice=format!("[Output truncated: {} of {} shown ({} limit). Re-fetch a more specific URL or use web_search for targeted content.]",format_byte_size(output_bytes),format_byte_size(total_bytes),format_byte_size(DEFAULT_OUTPUT_MAX_BYTES));
    WebfetchOutputCap{text:head,notice:Some(notice),truncated:true,output_bytes,total_bytes}
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn small_output_is_unchanged() { assert_eq!(cap_webfetch_output("hello"),WebfetchOutputCap{text:"hello".into(),notice:None,truncated:false,output_bytes:5,total_bytes:5}); }
    #[test] fn whole_lines_are_kept() { let input=format!("first\n{}", "x".repeat(DEFAULT_OUTPUT_MAX_BYTES)); let result=cap_webfetch_output(&input); assert_eq!(result.text,"first"); assert_eq!(result.output_bytes,5); assert!(result.truncated); }
    #[test] fn oversized_first_line_keeps_utf8_safe_prefix() { let result=cap_webfetch_output(&"한".repeat(DEFAULT_OUTPUT_MAX_BYTES)); assert_eq!(result.output_bytes,DEFAULT_OUTPUT_MAX_BYTES-2); assert!(result.text.chars().all(|c|c=='한')); }
    #[test] fn exact_ceiling_is_not_truncated() { assert!(!cap_webfetch_output(&"x".repeat(DEFAULT_OUTPUT_MAX_BYTES)).truncated); }
    #[test] fn unknown_format_defaults_to_markdown() { assert_eq!(parse_webfetch_format(Some("invalid")),WebfetchFormat::Markdown); assert_eq!(parse_webfetch_format(None),WebfetchFormat::Markdown); }
}
