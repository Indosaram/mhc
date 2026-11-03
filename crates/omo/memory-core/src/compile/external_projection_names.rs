//! Directory item naming for the external projection: lists every meaningful name, folds only
//! unmistakable machine ids and groups date-suffixed names.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use icu_collator::options::CollatorOptions;
use icu_collator::preferences::CollationNumericOrdering;
use icu_collator::{CollatorBorrowed, CollatorPreferences};
use regex::Regex;

use crate::sync::redact::redact_secret_like_material;

/// One file (or, after a directory merge, one name shared by `files` files) and its newest commit time.
#[derive(Debug, Clone, PartialEq)]
pub struct NamedFile {
    pub name: String,
    pub time: f64,
    pub files: usize,
}

/// One listed entry of a directory line: a name, a date group, or a pattern standing for hint-free names.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionItem {
    pub text: String,
    /// Files the entry stands for.
    pub files: usize,
    /// Newest commit time among those files; items are listed newest first.
    pub time: f64,
}

/// How a listed name maps back to its file; the block opens with it when nothing is folded or grouped.
pub const NAME_NOTE: &str = "Names drop \".md\" (a quoted name is exact).";

/// How a listed entry maps back to its file; the block opens with it when a line folds or groups names.
pub const NAME_LEGEND: &str = "Names drop \".md\" (a quoted name is exact); {a, b}-20261027 is a-20261027 and b-20261027; \"x … y (N files)\" and <id> stand for similar names: read the directory to list them.";

const MARKDOWN: &str = ".md";
const MIN_HEX_FLIPS: usize = 4;
// Each bar is met by most random base62 tokens; the most id-like of 13,995 real identifiers misses one by 0.05 or more.
const MIN_NOISE: f64 = 0.25;
const MIN_RARE_PAIRS: f64 = 0.3;
const MIN_WORD_DENSITY: f64 = 0.4;
const MIN_RUN: usize = 3;
const ID: &str = "<id>";
// The most frequent English letter pairs; the words of a random token are mostly made of other pairs.
const COMMON_PAIRS: &str = "th he in er an re on at en nd ti es or te of ed is it al ar st to nt ng se ha as ou io le ve co me de hi ri ro ic ne ea ra ce li ch ll be ma si om ur ca el ta la ns di fo ho pe ec pr no ct us ac ot il tr ly nc et ut ss so rs un lo wa ge ie wh ee wi em ad ol rt po we na ul ni ts mo ow pa im mi ai sh ir su id os iv ia am fi ci vi pl ig tu ev ld ry mp fe bl ab gh ty op wo sa ay ex ke fr oo av ag if ap gr od bo sp rd do uc bu ei ov by rm ep tt oc fa ef cu rn sc gi da yo cr cl du ga qu ue ff ba ey ls va um pp ua up lu go ht ru ug ds lt pi rc rr eg au ck ew mu br bi pt ak pu ui rg ib tl ny ki rk ys ob mm fu ph og ms ye ud mb ip ub oi rl gu dr hr cc tw ft wn nu af hu nn eo vo rv nf xp gn sm fl iz ok nl my gl aw sy oa";

fn re(cell: &'static OnceLock<Regex>, source: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(source).expect("static pattern"))
}

fn date_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    re(&CELL, r"^(?:19|20)[0-9]{6}$")
}

fn year_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    re(&CELL, r"^(?:19|20)[0-9]{2}$")
}

fn month_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    re(&CELL, r"^(?:0[1-9]|1[0-2])$")
}

// A token after a date that names no topic: a clock time with an optional real zone (0930,
// 093015z, 0330KST), a one- or two-digit sequence number, or a part (p3). Any other number (a
// ticket, a PR, an HTTP code) or a clock time with a word after it (1200fix) is the hint and
// keeps the name listed.
fn hint_free_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    re(
        &CELL,
        r"(?i)^(?:(?:[01][0-9]|2[0-3])[0-5][0-9](?:[0-5][0-9])?(?:z|utc|gmt|kst|jst|pst|pdt|est|edt|cet|cest|bst|ist)?|[0-9]{1,2}|p[0-9]+)$",
    )
}

fn date_suffix_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    re(&CELL, r"^(.+)-((?:19|20)[0-9]{6})$")
}

fn uuid_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    re(&CELL, r"(?i)^[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$")
}

fn hex_digest_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    re(&CELL, r"^(?:[0-9a-f]{16,}|[0-9A-F]{16,})$")
}

fn brace_or_comma_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    re(&CELL, r"[{}]|, ")
}

fn common_pairs() -> &'static HashSet<&'static str> {
    static CELL: OnceLock<HashSet<&'static str>> = OnceLock::new();
    CELL.get_or_init(|| COMMON_PAIRS.split(' ').collect())
}

fn collate(numeric: bool, a: &str, b: &str) -> Ordering {
    thread_local! {
        static PLAIN: CollatorBorrowed<'static> =
            CollatorBorrowed::try_new(CollatorPreferences::default(), CollatorOptions::default())
                .expect("compiled collation data");
        static NUMERIC: CollatorBorrowed<'static> = {
            let mut prefs = CollatorPreferences::default();
            prefs.numeric_ordering = Some(CollationNumericOrdering::True);
            CollatorBorrowed::try_new(prefs, CollatorOptions::default())
                .expect("compiled collation data")
        };
    }
    if numeric {
        NUMERIC.with(|collator| collator.compare(a, b))
    } else {
        PLAIN.with(|collator| collator.compare(a, b))
    }
}

/// `a.localeCompare(b)`.
pub fn locale_compare(a: &str, b: &str) -> Ordering {
    collate(false, a, b)
}

/// `a.localeCompare(b, "en", { numeric: true })`.
fn naturally(a: &str, b: &str) -> Ordering {
    collate(true, a, b)
}

fn newest_first(a: &ProjectionItem, b: &ProjectionItem) -> Ordering {
    let delta = b.time - a.time;
    if delta != 0.0 && !delta.is_nan() {
        // A negative delta lists `a` first.
        return delta.partial_cmp(&0.0).unwrap_or(Ordering::Equal);
    }
    locale_compare(&a.text, &b.text)
}

/// The names in `names` that are unmistakably machine ids, each judged on its own: a sibling never
/// pulls a name in. When unsure a name is shown, since the name is the hint.
pub fn generated_ids<S: AsRef<str>>(names: &[S]) -> HashSet<String> {
    names
        .iter()
        .map(AsRef::as_ref)
        .filter(|name| uuid_re().is_match(name) || is_hex_digest(name) || reads_as_random(name))
        .map(str::to_string)
        .collect()
}

fn is_hex_digest(name: &str) -> bool {
    if !hex_digest_re().is_match(name) {
        return false;
    }
    let bytes = name.as_bytes();
    let flips = bytes
        .windows(2)
        .filter(|pair| pair[0].is_ascii_digit() != pair[1].is_ascii_digit())
        .count();
    flips >= MIN_HEX_FLIPS
}

/// The camel-case words of a base62 token, as the TS pattern
/// `[A-Z]+(?=[A-Z][a-z])|[A-Z]?[a-z]+|[A-Z]+|\d+` reads it.
fn words(name: &str) -> Vec<&str> {
    let bytes = name.as_bytes();
    let mut words = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        let run = |from: usize, test: fn(&u8) -> bool| {
            from + bytes[from..].iter().take_while(|b| test(b)).count()
        };
        let end = if byte.is_ascii_uppercase() {
            let upper_end = run(at, u8::is_ascii_uppercase);
            let next_lower = bytes.get(upper_end).is_some_and(u8::is_ascii_lowercase);
            if upper_end - at >= 2 && next_lower {
                upper_end - 1
            } else if upper_end - at == 1 && next_lower {
                run(at + 1, u8::is_ascii_lowercase)
            } else {
                upper_end
            }
        } else if byte.is_ascii_lowercase() {
            run(at, u8::is_ascii_lowercase)
        } else if byte.is_ascii_digit() {
            run(at, u8::is_ascii_digit)
        } else {
            at + 1
        };
        if byte.is_ascii_alphanumeric() {
            words.push(&name[at..end]);
        }
        at = end;
    }
    words
}

/// A mixed-case base62 token of 12+ characters that reads as random by all three measures of its
/// camel-case words: noise marks (a lone letter, a one- or two-digit chunk, a word with no vowel:
/// two marks from four letters up) per character, the share of uncommon letter pairs inside
/// words, and words per character.
fn reads_as_random(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.len() < 12
        || !bytes.iter().all(u8::is_ascii_alphanumeric)
        || !bytes.iter().any(|b| b.is_ascii_lowercase())
        || !bytes.iter().any(|b| b.is_ascii_uppercase())
    {
        return false;
    }
    let words = words(name);
    let (mut marks, mut pairs, mut rare) = (0usize, 0usize, 0usize);
    for word in &words {
        if word.bytes().all(|b| b.is_ascii_digit()) {
            marks += usize::from(word.len() <= 2);
            continue;
        }
        if word.len() == 1 {
            marks += 1;
        } else if !word.bytes().any(|b| b"aeiouyAEIOUY".contains(&b)) {
            marks += if word.len() >= 4 { 2 } else { 1 };
        }
        let lower = word.to_ascii_lowercase();
        for index in 1..lower.len() {
            pairs += 1;
            if !common_pairs().contains(&lower[index - 1..=index]) {
                rare += 1;
            }
        }
    }
    let length = name.len() as f64;
    marks as f64 / length >= MIN_NOISE
        && pairs > 0
        && rare as f64 / pairs as f64 >= MIN_RARE_PAIRS
        && words.len() as f64 / length >= MIN_WORD_DENSITY
}

/// `a.md` lists as `a`; any other file keeps its exact name in quotes. Secret-like text is masked first.
fn display_name(name: &str) -> String {
    let safe = redact_secret_like_material(name);
    match safe.strip_suffix(MARKDOWN) {
        Some(stem) => stem.to_string(),
        None => format!("\"{safe}\""),
    }
}

/// Insertion-ordered grouping, like a JS `Map` of arrays.
struct Groups<T> {
    index: HashMap<String, usize>,
    rows: Vec<(String, Vec<T>)>,
}

impl<T> Groups<T> {
    fn new() -> Self {
        Self {
            index: HashMap::new(),
            rows: Vec::new(),
        }
    }

    fn push(&mut self, key: &str, value: T) {
        match self.index.get(key) {
            Some(at) => self.rows[*at].1.push(value),
            None => {
                self.index.insert(key.to_string(), self.rows.len());
                self.rows.push((key.to_string(), vec![value]));
            }
        }
    }
}

/// One directory's files as listed items, newest first. Hint-free runs (a shared prefix plus only
/// a date, month chunk, clock time or part; or generated ids) of at least three fold into one
/// pattern when that is shorter than the list. The other markdown names sharing a `-YYYYMMDD`
/// suffix are grouped as `{a, b}-YYYYMMDD`.
pub fn directory_items(files: &[NamedFile]) -> Vec<ProjectionItem> {
    let merged = merge_by_name(files);
    let stems: Vec<&str> = merged
        .iter()
        .filter(|file| is_markdown(file))
        .map(|file| stem(&file.name))
        .collect();
    let ids = generated_ids(&stems);
    let mut runs: Groups<NamedFile> = Groups::new();
    let mut plain: Vec<NamedFile> = Vec::new();
    for file in &merged {
        match run_key(&file.name, &ids) {
            Some(key) => runs.push(&key, file.clone()),
            None => plain.push(file.clone()),
        }
    }
    let mut items = Vec::new();
    for (key, members) in runs.rows {
        match fold_run(&key, &members) {
            Some(folded) => items.push(folded),
            None => plain.extend(members),
        }
    }
    items.extend(date_groups(&plain));
    items.sort_by(newest_first);
    items
}

fn merge_by_name(files: &[NamedFile]) -> Vec<NamedFile> {
    let mut merged: Vec<NamedFile> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for file in files {
        let name = redact_secret_like_material(&file.name);
        match seen.get(&name) {
            Some(at) => {
                let row = &mut merged[*at];
                row.time = row.time.max(file.time);
                row.files += file.files;
            }
            None => {
                seen.insert(name.clone(), merged.len());
                merged.push(NamedFile {
                    name,
                    time: file.time,
                    files: file.files,
                });
            }
        }
    }
    merged
}

fn is_markdown(file: &NamedFile) -> bool {
    file.name.ends_with(MARKDOWN)
}

fn stem(name: &str) -> &str {
    &name[..name.len() - MARKDOWN.len()]
}

fn item(file: &NamedFile) -> ProjectionItem {
    ProjectionItem {
        text: display_name(&file.name),
        files: file.files,
        time: file.time,
    }
}

fn newest(files: &[NamedFile]) -> f64 {
    files
        .iter()
        .map(|file| file.time)
        .fold(f64::NEG_INFINITY, f64::max)
}

fn count(files: &[NamedFile]) -> usize {
    files.iter().map(|file| file.files).sum()
}

/// What a hint-free name shares with its run (`<id>`, or the words before its date), or `None`
/// for a hinted name.
fn run_key(name: &str, ids: &HashSet<String>) -> Option<String> {
    if !name.ends_with(MARKDOWN) {
        return None;
    }
    let stem = stem(name);
    if ids.contains(stem) {
        return Some(ID.to_string());
    }
    let tokens: Vec<&str> = stem.split('-').collect();
    for (index, token) in tokens.iter().enumerate() {
        let date_length = if date_re().is_match(token) {
            1
        } else if year_re().is_match(token)
            && month_re().is_match(tokens.get(index + 1).copied().unwrap_or(""))
        {
            2
        } else {
            0
        };
        if date_length == 0 {
            continue;
        }
        let tail = &tokens[index + date_length..];
        if !tail
            .iter()
            .all(|part| hint_free_re().is_match(part) || date_re().is_match(part))
        {
            return None;
        }
        return Some(format!("{}-", tokens[..index].join("-")));
    }
    None
}

fn fold_run(key: &str, members: &[NamedFile]) -> Option<ProjectionItem> {
    if members.len() < MIN_RUN {
        return None;
    }
    let files = count(members);
    let mut names: Vec<String> = members
        .iter()
        .map(|member| display_name(&member.name))
        .collect();
    names.sort_by(|a, b| naturally(a, b));
    let text = if key == ID {
        format!("{ID} ({files} files)")
    } else {
        format!("{} … {} ({files} files)", names[0], names[names.len() - 1])
    };
    let listed = names.join(", ").len();
    (text.len() < listed).then(|| ProjectionItem {
        text,
        files,
        time: newest(members),
    })
}

fn date_groups(files: &[NamedFile]) -> Vec<ProjectionItem> {
    let mut groups: Groups<NamedFile> = Groups::new();
    let mut items = Vec::new();
    for file in files {
        let date = (is_markdown(file) && !brace_or_comma_re().is_match(&file.name))
            .then(|| date_suffix_re().captures(stem(&file.name)))
            .flatten()
            .map(|caps| caps[2].to_string());
        match date {
            Some(date) => groups.push(&date, file.clone()),
            None => items.push(item(file)),
        }
    }
    for (date, members) in groups.rows {
        if members.len() < 2 {
            items.extend(members.iter().map(item));
            continue;
        }
        let mut entries: Vec<ProjectionItem> = members.iter().map(item).collect();
        entries.sort_by(newest_first);
        let listed: Vec<&str> = entries
            .iter()
            .map(|entry| &entry.text[..entry.text.len() - date.len() - 1])
            .collect();
        items.push(ProjectionItem {
            text: format!("{{{}}}-{date}", listed.join(", ")),
            files: count(&members),
            time: newest(&members),
        });
    }
    items
}

#[cfg(test)]
#[path = "external_projection_names_tests.rs"]
mod tests;
