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

/// senpi's `VERSION_RE`: a full semver string with an optional prerelease.
static VERSION_RE:LazyLock<Regex>=LazyLock::new(||Regex::new(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$").expect("version pattern"));
/// senpi's `CALVER_RE`: `YYYY.M.D` with an optional build counter from 2 up.
static CALVER_RE:LazyLock<Regex>=LazyLock::new(||Regex::new(r"^(\d{4})\.(\d{1,2})\.(\d{1,2})(?:-([2-9]\d*))?$").expect("calver pattern"));
/// senpi's `getNewEntries` last-version pattern.
static PLAIN_VERSION_RE:LazyLock<Regex>=LazyLock::new(||Regex::new(r"^(\d+)\.(\d+)\.(\d+)(?:-(.*))?$").expect("plain version pattern"));
/// senpi's `isForeignVersion` beta form.
static FOREIGN_BETA_RE:LazyLock<Regex>=LazyLock::new(||Regex::new(r"^\d+\.\d+\.\d+-0\.beta\.").expect("foreign beta pattern"));

/// senpi's `isForeignVersion`: versions from a foreign release track never compare.
pub fn is_foreign_version(version:&str)->bool {
    version.starts_with("0.0.0-omob.") || FOREIGN_BETA_RE.is_match(version)
}

/// senpi's `compareVersionStrings`; `None` is its `undefined` (incomparable versions).
pub fn compare_version_strings(left:&str,right:&str)->Option<i32> {
    if let (Some(left_calver),Some(right_calver))=(CALVER_RE.captures(left),CALVER_RE.captures(right)) {
        let parts=|capture:&regex::Captures<'_>|[
            capture[1].parse::<u64>().unwrap_or_default(),capture[2].parse::<u64>().unwrap_or_default(),capture[3].parse::<u64>().unwrap_or_default(),
            capture.get(4).map_or(1,|value|value.as_str().parse::<u64>().unwrap_or(1)),
        ];
        let (left_parts,right_parts)=(parts(&left_calver),parts(&right_calver));
        for index in 0..left_parts.len() {
            if left_parts[index]!=right_parts[index] { return Some(if left_parts[index]<right_parts[index] { -1 } else { 1 }); }
        }
        return Some(0);
    }
    let (Ok(left_valid),Ok(right_valid))=(semver::Version::parse(left),semver::Version::parse(right)) else { return None; };
    if is_foreign_version(left)!=is_foreign_version(right) { return None; }
    Some(match left_valid.cmp(&right_valid) { std::cmp::Ordering::Less=>-1,std::cmp::Ordering::Equal=>0,std::cmp::Ordering::Greater=>1 })
}

/// senpi's `getNewEntries`: entries newer than `last_version`, capped at `current_version`
/// when one is given, keeping the first row for each version.
pub fn get_new_entries(entries:&[(String,String)],last_version:&str,current_version:Option<&str>)->Vec<(String,String)> {
    if !PLAIN_VERSION_RE.is_match(last_version) || is_foreign_version(last_version) { return Vec::new(); }
    let current=current_version.filter(|version|VERSION_RE.is_match(version));
    if let Some(current)=current && (is_foreign_version(current) || compare_version_strings(last_version,current).is_none()) { return Vec::new(); }
    let mut seen=std::collections::HashSet::new();
    entries.iter().filter(|(version,_)| {
        if !seen.insert(version.clone()) { return false; }
        let Some(lower)=compare_version_strings(version,last_version) else { return false; };
        lower>0 && current.is_none_or(|current|compare_version_strings(version,current).is_none_or(|upper|upper<=0))
    }).cloned().collect()
}

/// senpi's `getChangelogForDisplay` decision: the startup markdown and the version to record.
#[derive(Debug,Clone,Default,PartialEq,Eq)]
pub struct ChangelogDisplay {
    pub markdown: Option<String>,
    pub record_version: Option<String>,
}

/// senpi's `getChangelogForDisplay`: skip resumed sessions, record the first-seen version on a
/// fresh install, and return only entries newer than the last seen version otherwise.
pub fn changelog_for_display(has_messages:bool,source:&maho_core::changelog_source::ChangelogSource,last_version:Option<&str>,entries:&[(String,String)])->ChangelogDisplay {
    if has_messages { return ChangelogDisplay::default(); }
    let Some(version)=source.version.clone() else { return ChangelogDisplay::default(); };
    let Some(last_version)=last_version else { return ChangelogDisplay { markdown:None,record_version:Some(version) }; };
    let new_entries=get_new_entries(entries,last_version,Some(&version));
    if new_entries.is_empty() { return ChangelogDisplay::default(); }
    let markdown=new_entries.iter().map(|(entry_version,content)|if source.rewrite_links {normalize_links(content,entry_version)}else{content.clone()}).collect::<Vec<_>>().join("\n\n");
    ChangelogDisplay { markdown:Some(markdown),record_version:Some(version) }
}

/// senpi's startup-notice version probe (`changelogMarkdown.match(/##\s+\[?(\d+\.\d+\.\d+)\]?/)`):
/// the first `## x.y.z` header in the markdown.
pub fn latest_version_in_markdown(markdown:&str)->Option<String> {
    static HEADER:LazyLock<Regex>=LazyLock::new(||Regex::new(r"##\s+\[?(\d+\.\d+\.\d+)\]?").expect("changelog header pattern"));
    HEADER.captures(markdown).map(|capture|capture[1].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn package_links_are_pinned_and_external_links_stay_external() {
        assert_eq!(normalize_links("[doc](README.md#trust) [dir](examples/) [web](https://example.test)","0.79.0"),"[doc](https://github.com/earendil-works/pi/blob/v0.79.0/packages/coding-agent/README.md#trust) [dir](https://github.com/earendil-works/pi/tree/v0.79.0/packages/coding-agent/examples/) [web](https://example.test)");
    }

    #[test]
    fn latest_version_probe_reads_the_first_header() {
        assert_eq!(latest_version_in_markdown("## 0.80.0 - 2026-01-02\nbody"),Some("0.80.0".to_owned()));
        assert_eq!(latest_version_in_markdown("## [0.81.0]\nbody"),Some("0.81.0".to_owned()));
        assert_eq!(latest_version_in_markdown("no header here"),None);
    }

    fn entries()->Vec<(String,String)> {
        vec![("0.81.0".to_owned(),"e81".to_owned()),("0.80.0".to_owned(),"e80".to_owned()),("0.79.0".to_owned(),"e79".to_owned())]
    }

    #[test]
    fn new_entries_follow_the_last_seen_version_and_the_current_ceiling() {
        assert!(get_new_entries(&entries(),"0.81.0",Some("0.81.0")).is_empty(),"nothing is newer than the seen version");
        let bounded=get_new_entries(&entries(),"0.79.0",Some("0.80.0"));
        assert_eq!(bounded.iter().map(|(version,_)|version.as_str()).collect::<Vec<_>>(),["0.80.0"],"the current build caps the range");
        let all=get_new_entries(&entries(),"0.79.0",None);
        assert_eq!(all.iter().map(|(version,_)|version.as_str()).collect::<Vec<_>>(),["0.81.0","0.80.0"]);
        assert!(get_new_entries(&entries(),"0.0.0-omob.1",None).is_empty(),"a foreign version never seeds the comparison");
        assert!(get_new_entries(&entries(),"not-a-version",None).is_empty());
    }

    #[test]
    fn calver_and_prerelease_versions_compare_like_the_source() {
        assert_eq!(compare_version_strings("2026.10.3","2026.10.3"),Some(0));
        assert_eq!(compare_version_strings("2026.10.3-2","2026.10.3"),Some(1),"the calendar build counter breaks the tie");
        assert_eq!(compare_version_strings("0.80.0","0.79.0"),Some(1));
        assert_eq!(compare_version_strings("1.0.0-rc.1","1.0.0"),Some(-1),"a prerelease precedes its release");
        assert_eq!(compare_version_strings("0.0.0-omob.1","1.0.0"),None,"a foreign version never compares");
    }

    #[test]
    fn display_skips_resumed_sessions_and_records_the_first_seen_version() {
        let source=maho_core::changelog_source::ChangelogSource { id:"engine".into(),path:"/tmp/CHANGELOG.md".into(),version:Some("0.80.0".into()),rewrite_links:true };
        assert!(changelog_for_display(true,&source,Some("0.79.0"),&entries()).markdown.is_none(),"a resumed session shows nothing");
        let fresh=changelog_for_display(false,&source,None,&entries());
        assert_eq!(fresh.record_version.as_deref(),Some("0.80.0"));
        assert!(fresh.markdown.is_none(),"a fresh install records without showing");
        let seen=changelog_for_display(false,&source,Some("0.79.0"),&[("0.80.0".into(),"[doc](README.md)".into())]);
        assert!(seen.markdown.as_deref().is_some_and(|markdown|markdown.contains("blob/v0.80.0/packages/coding-agent/README.md")),"new entries render with pinned links");
        assert_eq!(seen.record_version.as_deref(),Some("0.80.0"));
        assert!(changelog_for_display(false,&source,Some("0.80.0"),&entries()).markdown.is_none(),"an up-to-date install shows nothing");
    }
}
