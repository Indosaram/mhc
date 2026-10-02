pub const SLEEP_WAIT_THRESHOLD_SECONDS:f64=10.0;
pub const SLEEP_WAIT_LOOP_THRESHOLD_SECONDS:f64=2.0;

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum SleepWaitRule {R1,R2,R3,R4}
#[derive(Clone,Debug,PartialEq)]
pub struct SleepWaitClassification {pub rule:SleepWaitRule,pub seconds:f64}

pub fn classify_sleep_wait(command:&str)->Result<Option<SleepWaitClassification>,Box<fancy_regex::Error>> {
    let power=fancy_regex::Regex::new(r"\b(?:pmset|systemsetup|caffeinate|displaysleep|disksleep)\b")?;
    if power.is_match(command)? {return Ok(None);}
    let wrapper=fancy_regex::Regex::new(r"(?i)^(?:env\s+(?:[\w.]+=[^\s]*\s+)*)?(?:/(?:usr/)?bin/)?(?:ba|z|da|k)?sh\s+-[a-z]*c\s+")?;
    let mut current=command.trim();
    for _ in 0..3 {
        let Some(found)=wrapper.find(current)? else {break;};
        current=current[found.end()..].trim();
        if current.len()>1 && (current.starts_with('\'') && current.ends_with('\'') || current.starts_with('"') && current.ends_with('"')) {current=current[1..current.len()-1].trim();}
    }
    let sleep=fancy_regex::Regex::new(r"(?<![\w.\-/])(?:/(?:usr/)?bin/)?sleep\s+(\d+(?:\.\d+)?)")?;
    let mut longest:Option<f64>=None;
    for captures in sleep.captures_iter(current) {
        let captures=captures?;
        if let Some(seconds)=captures.get(1).and_then(|m|m.as_str().parse::<f64>().ok()).filter(|v|v.is_finite()) {longest=Some(longest.map_or(seconds,|v|v.max(seconds)));}
    }
    let Some(seconds)=longest else {return Ok(None);};
    let looping=fancy_regex::Regex::new(r"\b(?:while|until|for)\b[\s\S]*?(?<![\w.\-/])(?:/(?:usr/)?bin/)?sleep\s+\d")?;
    if looping.is_match(current)? {return Ok((seconds>=SLEEP_WAIT_LOOP_THRESHOLD_SECONDS).then_some(SleepWaitClassification {rule:SleepWaitRule::R3,seconds}));}
    if seconds<SLEEP_WAIT_THRESHOLD_SECONDS {return Ok(None);}
    for (rule,pattern) in [
        (SleepWaitRule::R1,r"^sleep\s+\d+(?:\.\d+)?$"),
        (SleepWaitRule::R2,r"^\(?\s*(?:/(?:usr/)?bin/)?sleep\s+\d+(?:\.\d+)?\s*(?:[;&|]|$)"),
        (SleepWaitRule::R4,r"[;&|]\s*(?:/(?:usr/)?bin/)?sleep\s+\d+(?:\.\d+)?\s*\)?\s*$"),
    ] {if fancy_regex::Regex::new(pattern)?.is_match(current)? {return Ok(Some(SleepWaitClassification {rule,seconds}));}}
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn assert_case(command:&str,expected:Option<(SleepWaitRule,f64)>) {assert_eq!(classify_sleep_wait(command).expect("classification"),expected.map(|(rule,seconds)|SleepWaitClassification {rule,seconds}));}
    #[test] fn pure_long_sleep() {assert_case("sleep 270",Some((SleepWaitRule::R1,270.0)));}
    #[test] fn short_settle_sleep() {assert_case("sleep 3",None);}
    #[test] fn exact_threshold() {assert_case("sleep 10",Some((SleepWaitRule::R1,10.0)));}
    #[test] fn canonical_leading_check() {assert_case("sleep 270; git log --oneline -2",Some((SleepWaitRule::R2,270.0)));}
    #[test] fn and_chain() {assert_case("sleep 45 && gh pr view 6127 --json mergeStateStatus",Some((SleepWaitRule::R2,45.0)));}
    #[test] fn short_leading_settle() {assert_case("sleep 2 && curl -s http://localhost:3000/health",None);}
    #[test] fn for_loop_poll() {assert_case("for i in {1..6}; do kill -0 15598 || break; sleep 5; done",Some((SleepWaitRule::R3,5.0)));}
    #[test] fn while_poll() {assert_case("while true; do\n sleep 30\n ls /tmp/out || true\ndone",Some((SleepWaitRule::R3,30.0)));}
    #[test] fn subsecond_loop() {assert_case("for i in {1..5}; do sleep 0.1; done",None);}
    #[test] fn long_trailing() {assert_case("bun run dev & sleep 30",Some((SleepWaitRule::R4,30.0)));}
    #[test] fn short_trailing_settle() {assert_case("pkill -9 bun 2>/dev/null; sleep 1",None);}
    #[test] fn bash_wrapper() {assert_case("bash -lc 'sleep 270; git log --oneline -2'",Some((SleepWaitRule::R2,270.0)));}
    #[test] fn env_shell_wrapper() {assert_case("env FOO=1 sh -c \"sleep 120\"",Some((SleepWaitRule::R1,120.0)));}
    #[test] fn ordinary_command() {assert_case("npm run check",None);}
    #[test] fn sleep_like_token() {assert_case("./sleepless 300",None);}
    #[test] fn power_management_guard() {assert_case("pmset -g sleep 300",None);assert_case("caffeinate -i sleep 600",None);}
    #[test] fn longest_sleep_selected() {assert_case("sleep 15; echo mid; sleep 90",Some((SleepWaitRule::R2,90.0)));}
}
