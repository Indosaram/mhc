pub fn decode_html_entities(text:&str)->String{
    let mut output=String::new();let mut rest=text;
    while let Some(start)=rest.find('&'){
        output.push_str(&rest[..start]);rest=&rest[start..];
        let Some(end)=rest.find(';')else{output.push_str(rest);return output;};
        let entity=&rest[1..end];
        let decoded=if let Some(hex)=entity.strip_prefix("#x"){
            if !hex.is_empty()&&hex.chars().all(|ch|ch.is_ascii_hexdigit()){Some(decode_code_point(u32::from_str_radix(hex,16).ok()))}else{None}
        }else if let Some(decimal)=entity.strip_prefix('#'){
            if !decimal.is_empty()&&decimal.chars().all(|ch|ch.is_ascii_digit()){Some(decode_code_point(decimal.parse().ok()))}else{None}
        }else if !entity.is_empty()&&entity.chars().all(|ch|ch.is_ascii_alphabetic()){
            match entity.to_ascii_lowercase().as_str(){"amp"=>Some("&".into()),"apos"=>Some("'".into()),"gt"=>Some(">".into()),"lt"=>Some("<".into()),"nbsp"=>Some(" ".into()),"quot"=>Some("\"".into()),_=>Some(format!("&{entity};"))}
        }else{None};
        if let Some(decoded)=decoded{output.push_str(&decoded);rest=&rest[end+1..];}else{output.push('&');rest=&rest[1..];}
    }
    output.push_str(rest);output
}
fn decode_code_point(point:Option<u32>)->String{point.and_then(char::from_u32).map_or_else(String::new,|point|point.to_string())}
