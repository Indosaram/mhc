//! Port of the parts of the `diff` package that `components/diff.ts` and `tools/diff-render.ts` use:
//! `diffWords` (word tokenizer, Myers edit script, whitespace de-duplication).
use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub value: String,
    pub added: bool,
    pub removed: bool,
    pub count: usize,
}

const EXTENDED_WORD_CHARS: &str = concat!(
    "a-zA-Z0-9_",
    "\\u{AD}",
    "\\u{C0}-\\u{D6}",
    "\\u{D8}-\\u{F6}",
    "\\u{F8}-\\u{2C6}",
    "\\u{2C8}-\\u{2D7}",
    "\\u{2DE}-\\u{2FF}",
    "\\u{1E00}-\\u{1EFF}",
);

const JS_WHITESPACE: &str = "\\t\\n\\u{0B}\\u{0C}\\r \\u{A0}\\u{1680}\\u{2000}-\\u{200A}\\u{2028}\\u{2029}\\u{202F}\\u{205F}\\u{3000}\\u{FEFF}";

static TOKENIZE_INCLUDING_WHITESPACE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!("[{EXTENDED_WORD_CHARS}]+|[{JS_WHITESPACE}]+|[^{EXTENDED_WORD_CHARS}]"))
        .expect("tokenize pattern")
});

static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!("[{JS_WHITESPACE}]")).expect("ws pattern"));

static LEADING_WHITESPACE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("^[{JS_WHITESPACE}]*")).expect("leading ws pattern"));

static LEADING_WHITESPACE_RUN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("^[{JS_WHITESPACE}]+")).expect("leading ws run pattern"));

fn is_whitespace(text: &str) -> bool {
    WHITESPACE.is_match(text)
}

fn is_js_whitespace_char(character: char) -> bool {
    matches!(
        character,
        '\t' | '\n'
            | '\u{0B}'
            | '\u{0C}'
            | '\r'
            | ' '
            | '\u{A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_whitespace_char)
}

fn tokenize(value: &str) -> Vec<String> {
    let parts: Vec<String> = TOKENIZE_INCLUDING_WHITESPACE.find_iter(value).map(|m| m.as_str().to_owned()).collect();
    let mut tokens: Vec<String> = Vec::new();
    let mut prev_part: Option<&str> = None;
    for part in parts.iter().map(String::as_str) {
        if is_whitespace(part) {
            match prev_part {
                None => tokens.push(part.to_owned()),
                Some(_) => {
                    let last = tokens.pop().unwrap_or_default();
                    tokens.push(last + part);
                }
            }
        } else if prev_part.is_some_and(is_whitespace) {
            if tokens.last().map(String::as_str) == prev_part {
                let last = tokens.pop().unwrap_or_default();
                tokens.push(last + part);
            } else {
                tokens.push(prev_part.unwrap_or_default().to_owned() + part);
            }
        } else {
            tokens.push(part.to_owned());
        }
        prev_part = Some(part);
    }
    tokens
}

fn remove_empty(tokens: Vec<String>) -> Vec<String> {
    tokens.into_iter().filter(|token| !token.is_empty()).collect()
}

fn join_tokens(tokens: &[String]) -> String {
    let mut out = String::new();
    for (index, token) in tokens.iter().enumerate() {
        if index == 0 {
            out.push_str(token);
        } else {
            out.push_str(&LEADING_WHITESPACE_RUN.replace(token, ""));
        }
    }
    out
}

fn longest_common_prefix(left: &str, right: &str) -> String {
    let mut end = 0;
    let mut left_chars = left.chars();
    let mut right_chars = right.chars();
    loop {
        match (left_chars.next(), right_chars.next()) {
            (Some(a), Some(b)) if a == b => end += a.len_utf8(),
            _ => break,
        }
    }
    left[..end].to_owned()
}

fn longest_common_suffix(left: &str, right: &str) -> String {
    if left.is_empty() || right.is_empty() || left.chars().next_back() != right.chars().next_back() {
        return String::new();
    }
    let mut length = 0;
    let mut left_chars = left.chars().rev();
    let mut right_chars = right.chars().rev();
    loop {
        match (left_chars.next(), right_chars.next()) {
            (Some(a), Some(b)) if a == b => length += a.len_utf8(),
            _ => break,
        }
    }
    left[left.len() - length..].to_owned()
}

fn replace_prefix(text: &str, old_prefix: &str, new_prefix: &str) -> String {
    format!("{new_prefix}{}", &text[old_prefix.len()..])
}

fn replace_suffix(text: &str, old_suffix: &str, new_suffix: &str) -> String {
    if old_suffix.is_empty() {
        return format!("{text}{new_suffix}");
    }
    format!("{}{new_suffix}", &text[..text.len() - old_suffix.len()])
}

fn remove_prefix(text: &str, old_prefix: &str) -> String {
    replace_prefix(text, old_prefix, "")
}

fn remove_suffix(text: &str, old_suffix: &str) -> String {
    replace_suffix(text, old_suffix, "")
}

fn overlap_count(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut start_a = 0;
    if a.len() > b.len() {
        start_a = a.len() - b.len();
    }
    let mut end_b = b.len();
    if a.len() < b.len() {
        end_b = a.len();
    }
    if end_b == 0 {
        return 0;
    }
    let mut map = vec![0usize; end_b];
    let mut k = 0usize;
    for j in 1..end_b {
        map[j] = if b[j] == b[k] { map[k] } else { k };
        while k > 0 && b[j] != b[k] {
            k = map[k];
        }
        if k < b.len() && b[j] == b[k] {
            k += 1;
        }
    }
    k = 0;
    for &item in &a[start_a..] {
        while k > 0 && item != b[k] {
            k = map[k];
        }
        if k < b.len() && item == b[k] {
            k += 1;
        }
    }
    k
}

fn maximum_overlap(left: &str, right: &str) -> String {
    let count = overlap_count(left, right);
    right.chars().take(count).collect()
}

fn trailing_ws(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut index = chars.len();
    while index > 0 && is_whitespace(&chars[index - 1].to_string()) {
        index -= 1;
    }
    chars[index..].iter().collect()
}

fn leading_ws(text: &str) -> String {
    LEADING_WHITESPACE.find(text).map(|m| m.as_str().to_owned()).unwrap_or_default()
}

fn leading_and_trailing_ws(text: &str) -> (String, String) {
    (leading_ws(text), trailing_ws(text))
}

struct Component {
    count: usize,
    added: bool,
    removed: bool,
    previous: Option<usize>,
}

#[derive(Clone, Copy)]
struct PathState {
    old_pos: i64,
    last_component: Option<usize>,
}

struct Differ {
    arena: Vec<Component>,
    best_path: HashMap<i64, PathState>,
    new_tokens: Vec<String>,
    old_tokens: Vec<String>,
}

impl Differ {
    fn push_component(&mut self, count: usize, added: bool, removed: bool, previous: Option<usize>) -> usize {
        self.arena.push(Component { count, added, removed, previous });
        self.arena.len() - 1
    }

    fn equals(&self, left: &str, right: &str) -> bool {
        js_trim(left) == js_trim(right)
    }

    fn add_to_path(&mut self, path: PathState, added: bool, removed: bool, old_pos_inc: i64) -> PathState {
        let last = path.last_component;
        if let Some(index) = last
            && self.arena[index].added == added
            && self.arena[index].removed == removed
        {
            let previous = self.arena[index].previous;
            let count = self.arena[index].count + 1;
            let component = self.push_component(count, added, removed, previous);
            return PathState { old_pos: path.old_pos + old_pos_inc, last_component: Some(component) };
        }
        let component = self.push_component(1, added, removed, last);
        PathState { old_pos: path.old_pos + old_pos_inc, last_component: Some(component) }
    }

    fn extract_common(&mut self, mut base_path: PathState, diagonal_path: i64) -> (PathState, i64) {
        let new_len = self.new_tokens.len() as i64;
        let old_len = self.old_tokens.len() as i64;
        let mut old_pos = base_path.old_pos;
        let mut new_pos = old_pos - diagonal_path;
        let mut common_count = 0usize;
        while new_pos + 1 < new_len
            && old_pos + 1 < old_len
            && self.equals(
                &self.old_tokens[(old_pos + 1) as usize],
                &self.new_tokens[(new_pos + 1) as usize],
            )
        {
            new_pos += 1;
            old_pos += 1;
            common_count += 1;
        }
        if common_count > 0 {
            let previous = base_path.last_component;
            base_path.last_component = Some(self.push_component(common_count, false, false, previous));
        }
        base_path.old_pos = old_pos;
        (base_path, new_pos)
    }

    fn build_values(&self, last_component: Option<usize>) -> Vec<Change> {
        let mut components: Vec<usize> = Vec::new();
        let mut next = last_component;
        while let Some(index) = next {
            components.push(index);
            next = self.arena[index].previous;
        }
        components.reverse();

        let mut changes = Vec::with_capacity(components.len());
        let mut new_pos = 0usize;
        let mut old_pos = 0usize;
        for index in components {
            let component = &self.arena[index];
            let value = if component.removed {
                let value = join_tokens(&self.old_tokens[old_pos..old_pos + component.count]);
                old_pos += component.count;
                value
            } else {
                let value = join_tokens(&self.new_tokens[new_pos..new_pos + component.count]);
                new_pos += component.count;
                if !component.added {
                    old_pos += component.count;
                }
                value
            };
            changes.push(Change { value, added: component.added, removed: component.removed, count: component.count });
        }
        changes
    }

    fn diff_with_options(&mut self) -> Vec<Change> {
        let new_len = self.new_tokens.len() as i64;
        let old_len = self.old_tokens.len() as i64;
        let mut edit_length = 1i64;
        let max_edit_length = new_len + old_len;
        let seed = PathState { old_pos: -1, last_component: None };
        self.best_path.insert(0, seed);
        let (seed, new_pos) = self.extract_common(seed, 0);
        self.best_path.insert(0, seed);
        if seed.old_pos + 1 >= old_len && new_pos + 1 >= new_len {
            return post_process(self.build_values(seed.last_component));
        }

        let mut min_diagonal = i64::MIN;
        let mut max_diagonal = i64::MAX;
        while edit_length <= max_edit_length {
            let mut diagonal = min_diagonal.max(-edit_length);
            while diagonal <= max_diagonal.min(edit_length) {
                let remove_path = self.best_path.get(&(diagonal - 1)).copied();
                let add_path = self.best_path.get(&(diagonal + 1)).copied();
                if remove_path.is_some() {
                    self.best_path.remove(&(diagonal - 1));
                }
                let can_add = add_path.is_some_and(|path| {
                    let add_path_new_pos = path.old_pos - diagonal;
                    0 <= add_path_new_pos && add_path_new_pos < new_len
                });
                let can_remove = remove_path.is_some_and(|path| path.old_pos + 1 < old_len);
                if !can_add && !can_remove {
                    self.best_path.remove(&diagonal);
                    diagonal += 2;
                    continue;
                }
                let base_path = if !can_remove
                    || (can_add && remove_path.map(|p| p.old_pos) < add_path.map(|p| p.old_pos))
                {
                    self.add_to_path(add_path.expect("can_add"), true, false, 0)
                } else {
                    self.add_to_path(remove_path.expect("can_remove"), false, true, 1)
                };
                let (base_path, new_pos) = self.extract_common(base_path, diagonal);
                if base_path.old_pos + 1 >= old_len && new_pos + 1 >= new_len {
                    return post_process(self.build_values(base_path.last_component));
                }
                self.best_path.insert(diagonal, base_path);
                if base_path.old_pos + 1 >= old_len {
                    max_diagonal = max_diagonal.min(diagonal - 1);
                }
                if new_pos + 1 >= new_len {
                    min_diagonal = min_diagonal.max(diagonal + 1);
                }
                diagonal += 2;
            }
            edit_length += 1;
        }
        Vec::new()
    }
}

fn dedupe_whitespace_in_change_objects(
    start_keep: Option<&mut Change>,
    deletion: Option<&mut Change>,
    insertion: Option<&mut Change>,
    end_keep: Option<&mut Change>,
) {
    match (deletion, insertion) {
        (Some(deletion), Some(insertion)) => {
            let (old_ws_prefix, old_ws_suffix) = leading_and_trailing_ws(&deletion.value);
            let (new_ws_prefix, new_ws_suffix) = leading_and_trailing_ws(&insertion.value);
            if let Some(start_keep) = start_keep {
                let common_ws_prefix = longest_common_prefix(&old_ws_prefix, &new_ws_prefix);
                start_keep.value = replace_suffix(&start_keep.value, &new_ws_prefix, &common_ws_prefix);
                deletion.value = remove_prefix(&deletion.value, &common_ws_prefix);
                insertion.value = remove_prefix(&insertion.value, &common_ws_prefix);
            }
            if let Some(end_keep) = end_keep {
                let common_ws_suffix = longest_common_suffix(&old_ws_suffix, &new_ws_suffix);
                end_keep.value = replace_prefix(&end_keep.value, &new_ws_suffix, &common_ws_suffix);
                deletion.value = remove_suffix(&deletion.value, &common_ws_suffix);
                insertion.value = remove_suffix(&insertion.value, &common_ws_suffix);
            }
        }
        (None, Some(insertion)) => {
            if start_keep.is_some() {
                let ws = leading_ws(&insertion.value);
                insertion.value = insertion.value[ws.len()..].to_owned();
            }
            if let Some(end_keep) = end_keep {
                let ws = leading_ws(&end_keep.value);
                end_keep.value = end_keep.value[ws.len()..].to_owned();
            }
        }
        (Some(deletion), None) => {
            match (start_keep, end_keep) {
                (Some(start_keep), Some(end_keep)) => {
                    let new_ws_full = leading_ws(&end_keep.value);
                    let (del_ws_start, del_ws_end) = leading_and_trailing_ws(&deletion.value);
                    let new_ws_start = longest_common_prefix(&new_ws_full, &del_ws_start);
                    deletion.value = remove_prefix(&deletion.value, &new_ws_start);
                    let remaining = remove_prefix(&new_ws_full, &new_ws_start);
                    let new_ws_end = longest_common_suffix(&remaining, &del_ws_end);
                    deletion.value = remove_suffix(&deletion.value, &new_ws_end);
                    end_keep.value = replace_prefix(&end_keep.value, &new_ws_full, &new_ws_end);
                    let kept = new_ws_full.len() - new_ws_end.len();
                    start_keep.value = replace_suffix(&start_keep.value, &new_ws_full, &new_ws_full[..kept]);
                }
                (None, Some(end_keep)) => {
                    let end_keep_ws_prefix = leading_ws(&end_keep.value);
                    let deletion_ws_suffix = trailing_ws(&deletion.value);
                    let overlap = maximum_overlap(&deletion_ws_suffix, &end_keep_ws_prefix);
                    deletion.value = remove_suffix(&deletion.value, &overlap);
                }
                (Some(start_keep), None) => {
                    let start_keep_ws_suffix = trailing_ws(&start_keep.value);
                    let deletion_ws_prefix = leading_ws(&deletion.value);
                    let overlap = maximum_overlap(&start_keep_ws_suffix, &deletion_ws_prefix);
                    deletion.value = remove_prefix(&deletion.value, &overlap);
                }
                (None, None) => {}
            }
        }
        (None, None) => {}
    }
}

fn post_process(changes: Vec<Change>) -> Vec<Change> {
    let mut changes = changes;
    let mut last_keep: Option<usize> = None;
    let mut insertion: Option<usize> = None;
    let mut deletion: Option<usize> = None;

    let mut index = 0;
    while index < changes.len() {
        if changes[index].added {
            insertion = Some(index);
        } else if changes[index].removed {
            deletion = Some(index);
        } else {
            if insertion.is_some() || deletion.is_some() {
                dedupe_at(&mut changes, last_keep, deletion, insertion, Some(index));
            }
            last_keep = Some(index);
            insertion = None;
            deletion = None;
        }
        index += 1;
    }
    if insertion.is_some() || deletion.is_some() {
        dedupe_at(&mut changes, last_keep, deletion, insertion, None);
    }
    changes
}

fn dedupe_at(
    changes: &mut [Change],
    start_keep: Option<usize>,
    deletion: Option<usize>,
    insertion: Option<usize>,
    end_keep: Option<usize>,
) {
    let (deletion_ref, insertion_ref) = (deletion, insertion);
    let mut deletion_value = deletion_ref.map(|index| changes[index].clone());
    let mut insertion_value = insertion_ref.map(|index| changes[index].clone());
    let mut start_value = start_keep.map(|index| changes[index].clone());
    let mut end_value = end_keep.map(|index| changes[index].clone());
    dedupe_whitespace_in_change_objects(
        start_value.as_mut(),
        deletion_value.as_mut(),
        insertion_value.as_mut(),
        end_value.as_mut(),
    );
    if let (Some(index), Some(value)) = (deletion_ref, deletion_value) {
        changes[index] = value;
    }
    if let (Some(index), Some(value)) = (insertion_ref, insertion_value) {
        changes[index] = value;
    }
    if let (Some(index), Some(value)) = (start_keep, start_value) {
        changes[index] = value;
    }
    if let (Some(index), Some(value)) = (end_keep, end_value) {
        changes[index] = value;
    }
}

pub fn diff_words(old_value: &str, new_value: &str) -> Vec<Change> {
    let old_tokens = remove_empty(tokenize(old_value));
    let new_tokens = remove_empty(tokenize(new_value));
    let mut differ = Differ { arena: Vec::new(), best_path: HashMap::new(), new_tokens, old_tokens };
    differ.diff_with_options()
}
