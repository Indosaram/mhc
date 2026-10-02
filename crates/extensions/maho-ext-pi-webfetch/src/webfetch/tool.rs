pub const DEFAULT_OUTPUT_MAX_BYTES:usize=50*1024;
pub fn parse_webfetch_format(value:Option<&serde_json::Value>)->super::fetcher::WebfetchFormat{
    match value.and_then(serde_json::Value::as_str){Some("text")=>super::fetcher::WebfetchFormat::Text,Some("html")=>super::fetcher::WebfetchFormat::Html,_=>super::fetcher::WebfetchFormat::Markdown}
}
#[derive(Debug,PartialEq,Eq)]
pub struct WebfetchOutputCap{pub text:String,pub truncated:bool,pub output_bytes:usize,pub total_bytes:usize}
pub fn cap_webfetch_output(text:&str)->WebfetchOutputCap{
    let total_bytes=text.len();
    if total_bytes<=DEFAULT_OUTPUT_MAX_BYTES{return WebfetchOutputCap{text:text.into(),truncated:false,output_bytes:total_bytes,total_bytes};}
    let mut lines=Vec::new();let mut used=0;
    for line in text.split('\n'){
        let bytes=line.len()+usize::from(!lines.is_empty());
        if used+bytes>DEFAULT_OUTPUT_MAX_BYTES{break;}
        lines.push(line);used+=bytes;
    }
    let head=if lines.is_empty(){
        let mut end=DEFAULT_OUTPUT_MAX_BYTES;while !text.is_char_boundary(end){end-=1;}
        let prefix=&text[..end];if end==DEFAULT_OUTPUT_MAX_BYTES{prefix.strip_suffix('\u{fffd}').unwrap_or(prefix).to_owned()}else{prefix.to_owned()}
    }else{lines.join("\n")};
    let output_bytes=head.len();
    let notice=format!("\n\n[Output truncated: {} of {} shown ({} limit). Re-fetch a more specific URL or use web_search for targeted content.]",format_byte_size(output_bytes),format_byte_size(total_bytes),format_byte_size(DEFAULT_OUTPUT_MAX_BYTES));
    WebfetchOutputCap{text:format!("{head}{notice}"),truncated:true,output_bytes,total_bytes}
}
fn format_byte_size(bytes:usize)->String{
    if bytes<1024{return format!("{bytes}B");}
    let number=bytes.to_string().parse::<f64>().unwrap_or_else(|_|unreachable!("usize decimal fits f64"));
    if bytes<1024*1024{format!("{:.1}KB",number/1024.0)}else{format!("{:.1}MB",number/(1024.0*1024.0))}
}
