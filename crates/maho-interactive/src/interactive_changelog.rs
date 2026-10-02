use std::sync::LazyLock;
use regex::Regex;

pub fn entries(path:&str)->Vec<(String,String)> {
    static HEADER:LazyLock<Regex>=LazyLock::new(||Regex::new(r"^##[ \t]+(?:\[([0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?)\]|([0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)))(?:[ \t]+-[ \t]+(\d{4}-\d{2}-\d{2}))?[ \t]*$").expect("version header"));
    static UNRELEASED:LazyLock<Regex>=LazyLock::new(||Regex::new(r"^##[ \t]+\[?Unreleased\]?[ \t]*(?:-[ \t]+\d{4}-\d{2}-\d{2})?[ \t]*$").expect("unreleased header"));
    static FENCE:LazyLock<Regex>=LazyLock::new(||Regex::new(r"^ {0,3}(`{3,}|~{3,})").expect("fence"));
    let Ok(content)=std::fs::read_to_string(path) else {return Vec::new();};
    let mut entries=Vec::new();let mut version=None;let mut lines=Vec::new();let mut fence:Option<(u8,usize)>=None;
    for line in content.split('\n') {
        if let Some(capture)=FENCE.captures(line) {let marker=capture[1].as_bytes();match fence {None=>fence=Some((marker[0],marker.len())),Some((character,length)) if character==marker[0] && marker.len()>=length=>fence=None,_=>{}}}
        let header=if fence.is_none(){HEADER.captures(line)}else{None};
        if header.is_some() || (fence.is_none() && UNRELEASED.is_match(line)) {
            if let Some(version)=version.take() && !lines.is_empty(){entries.push((version,lines.join("\n").trim().to_owned()));}
            lines.clear();
            version=header.and_then(|capture| {
                if capture.get(3).is_some_and(|date|chrono::NaiveDate::parse_from_str(date.as_str(),"%Y-%m-%d").is_err()){return None;}
                capture.get(1).or_else(||capture.get(2)).map(|version|version.as_str().to_owned())
            });
        } else if version.is_some(){lines.push(line);}
    }
    if let Some(version)=version && !lines.is_empty(){entries.push((version,lines.join("\n").trim().to_owned()));}
    entries
}

pub fn normalize_links(markdown:&str,version:&str)->String {
    static LINKS:LazyLock<Regex>=LazyLock::new(||Regex::new(r"(!?\[[^\]\n]+\]\()([^\s)]+)((?:\s+[^)]*)?\))").expect("markdown links"));
    static SCHEME:LazyLock<Regex>=LazyLock::new(||Regex::new(r"(?i)^[a-z][a-z0-9+.-]*:").expect("URL scheme"));
    static LEGACY:LazyLock<Regex>=LazyLock::new(||Regex::new(r"^https://github\.com/(?:badlogic|earendil-works)/pi-mono(?:/|$)").expect("legacy repository"));
    let tag=if version.starts_with('v'){version.to_owned()}else{format!("v{version}")};
    LINKS.replace_all(markdown,|capture:&regex::Captures<'_>| {
        let original=&capture[2];
        let mut target=LEGACY.replace(original,"https://github.com/earendil-works/pi/").into_owned();
        for route in ["blob","tree"] {for branch in ["main","master"] {let prefix=format!("https://github.com/earendil-works/pi/{route}/{branch}/");if let Some(path)=target.strip_prefix(&prefix){target=format!("https://github.com/earendil-works/pi/{route}/{tag}/{path}");}}}
        if !target.starts_with('#') && !target.starts_with("//") && !SCHEME.is_match(&target) {
            let end=target.find(['?','#']).unwrap_or(target.len());let path=&target[..end];
            if !path.is_empty(){
                let normalized=path.replace('\\',"/");let joined=if normalized.starts_with('/') {normalized.trim_start_matches('/').to_owned()}else{format!("packages/coding-agent/{normalized}")};
                let mut parts=Vec::new();let mut escaped=false;
                for part in joined.split('/') {match part {""|"."=>{},".."=>{if parts.pop().is_none(){escaped=true;}},part=>parts.push(part)}}
                if !escaped && !parts.is_empty(){
                    let route=if path.ends_with('/') || parts.last().is_some_and(|part|!part.contains('.')){"tree"}else{"blob"};
                    let repository=format!("{}{}",parts.join("/"),if normalized.ends_with('/') {"/"}else{""});let mut encoded=String::new();
                    for byte in repository.bytes(){if byte.is_ascii_alphanumeric() || b";/?:@&=+$,-_.!~*'()#".contains(&byte){encoded.push(char::from(byte));}else{encoded.push_str(&format!("%{byte:02X}"));}}
                    target=format!("https://github.com/earendil-works/pi/{route}/{tag}/{encoded}{}",&target[end..]);
                }
            }
        }
        format!("{}{}{}",&capture[1],target,&capture[3])
    }).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn package_links_are_pinned_and_external_links_stay_external() {
        assert_eq!(normalize_links("[doc](README.md#trust) [dir](examples/) [web](https://example.test)","0.79.0"),"[doc](https://github.com/earendil-works/pi/blob/v0.79.0/packages/coding-agent/README.md#trust) [dir](https://github.com/earendil-works/pi/tree/v0.79.0/packages/coding-agent/examples/) [web](https://example.test)");
    }
}
