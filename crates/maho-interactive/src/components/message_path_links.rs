//! Port of senpi packages/coding-agent/src/modes/interactive/components/message-path-links.ts.
//!
//! Links in a message whose target is a path (`[plan](docs/plan.md)`) become `file://` links to the
//! file the engine's one rule finds (`resolve_message_path`), so the terminal opens the same file the
//! desktop app and RPC clients do (#3415). Only when the terminal shows hyperlinks: otherwise the
//! link text would print a long absolute path. URLs, anchors, images, fenced and indented code and
//! inline code are left alone; a path that resolves to nothing keeps its text; a network or device
//! path is never resolved (#3419 review). Each answer, found or not, is remembered per working
//! directory, so a streaming message re-renders without touching the disk again.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::LazyLock;

use maho_core::message_path_resolution::{
    ResolvedMessagePath, is_local_message_path, message_path_project_root, resolve_message_path,
};
use regex::{Captures, Regex};

use maho_tui::terminal_image::path_to_file_url;

use super::markdown_transform::MarkdownTransformer;

static FENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^ {0,3}(`{3,}|~{3,})").expect("fence pattern"));
static INDENTED_CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?: {4}|\t)").expect("indented code pattern"));
static LINK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(!?)\[([^\]]*)\]\(\s*([^)\s]+)((?:\s+"[^"]*")?)\s*\)"#).expect("link pattern")
});
static INLINE_CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"`+[^`]*`+").expect("inline code pattern"));
static SCHEME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z][A-Za-z0-9+.-]*:").expect("scheme pattern"));
static DRIVE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z]:[\\/]").expect("drive pattern"));
static NETWORK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\\/]{2}").expect("network pattern"));
static ROOTED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[\\/]").expect("rooted pattern"));
const CACHE_LIMIT: usize = 256;

/// `resolveMessagePath({ path, cwd, projectRoot })`.
pub type MessagePathResolver = Rc<dyn Fn(&str, &str, Option<&str>) -> ResolvedMessagePath>;

#[derive(Default, Clone)]
pub struct MessagePathLinkOptions {
    pub hyperlinks: Option<Rc<dyn Fn() -> bool>>,
    pub resolve: Option<MessagePathResolver>,
}

/// Insertion-ordered cache that forgets its oldest answer past `CACHE_LIMIT` (senpi's `Map` LRU).
#[derive(Default)]
struct Answers {
    values: HashMap<String, Option<String>>,
    order: VecDeque<String>,
}

impl Answers {
    fn get(&self, key: &str) -> Option<Option<String>> {
        self.values.get(key).cloned()
    }

    fn remember(&mut self, key: String, url: Option<String>) {
        if self.values.remove(&key).is_some() {
            self.order.retain(|known| known != &key);
        }
        if self.values.len() >= CACHE_LIMIT
            && let Some(oldest) = self.order.pop_front()
        {
            self.values.remove(&oldest);
        }
        self.order.push_back(key.clone());
        self.values.insert(key, url);
    }
}

fn hex(byte: u8) -> Option<u8> {
    (byte as char)
        .to_digit(16)
        .and_then(|digit| u8::try_from(digit).ok())
}

/// JS `decodeURI`: `%XX` UTF-8 sequences decode, except those that decode to a reserved character
/// (`;/?:@&=+$,#`), which stay escaped; a malformed escape is an error (`URIError`).
pub(crate) fn decode_uri(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    let escaped = |at: usize| -> Option<u8> {
        if bytes.get(at) != Some(&b'%') {
            return None;
        }
        Some(hex(*bytes.get(at + 1)?)? << 4 | hex(*bytes.get(at + 2)?)?)
    };
    while index < bytes.len() {
        if bytes[index] != b'%' {
            out.push(bytes[index]);
            index += 1;
            continue;
        }
        let lead = escaped(index)?;
        let width = match lead {
            0x00..=0x7f => 1,
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return None,
        };
        if width == 1 {
            if b";/?:@&=+$,#".contains(&lead) {
                out.extend_from_slice(&bytes[index..index + 3]);
            } else {
                out.push(lead);
            }
            index += 3;
            continue;
        }
        let mut sequence = vec![lead];
        for part in 1..width {
            let byte = escaped(index + part * 3)?;
            if byte & 0xc0 != 0x80 {
                return None;
            }
            sequence.push(byte);
        }
        std::str::from_utf8(&sequence).ok()?;
        out.extend_from_slice(&sequence);
        index += width * 3;
    }
    String::from_utf8(out).ok()
}

pub fn create_message_path_link_transformer(
    get_cwd: Rc<dyn Fn() -> String>,
    options: MessagePathLinkOptions,
) -> MarkdownTransformer {
    let hyperlinks = options
        .hyperlinks
        .unwrap_or_else(|| Rc::new(|| maho_tui::terminal_image::get_capabilities().hyperlinks));
    let resolve_path: MessagePathResolver = options
        .resolve
        .unwrap_or_else(|| Rc::new(resolve_message_path));
    let answers = Rc::new(RefCell::new(Answers::default()));
    let project_roots: Rc<RefCell<HashMap<String, Option<String>>>> = Rc::default();

    let url_for = move |raw_path: &str, cwd: &str| -> Option<String> {
        let key = format!("{cwd}\0{raw_path}");
        let known = answers.borrow().get(&key);
        if let Some(known) = known {
            answers.borrow_mut().remember(key, known.clone());
            return known;
        }
        let Some(path) = decode_uri(raw_path) else {
            answers.borrow_mut().remember(key, None);
            return None;
        };
        if NETWORK.is_match(&path)
            || (ROOTED.is_match(&path) && !is_local_message_path(&path, cfg!(windows)))
        {
            answers.borrow_mut().remember(key, None);
            return None;
        }
        let project_root = project_roots
            .borrow_mut()
            .entry(cwd.to_owned())
            .or_insert_with(|| message_path_project_root(cwd))
            .clone();
        let url = match resolve_path(&path, cwd, project_root.as_deref()) {
            ResolvedMessagePath::Found { realpath, .. } => Some(path_to_file_url(&realpath)),
            ResolvedMessagePath::NotFound { .. } => None,
        };
        answers.borrow_mut().remember(key, url.clone());
        url
    };

    let rewrite_link = move |captures: &Captures<'_>, cwd: &str| -> String {
        let whole = &captures[0];
        let (bang, text, target, title) = (&captures[1], &captures[2], &captures[3], &captures[4]);
        if bang == "!" || target.starts_with('#') || target.starts_with('<') {
            return whole.to_owned();
        }
        if SCHEME.is_match(target) && !DRIVE.is_match(target) {
            return whole.to_owned();
        }
        let hash_at = target.find('#');
        let path = hash_at.map_or(target, |at| &target[..at]);
        let url = if path.is_empty() {
            None
        } else {
            url_for(path, cwd)
        };
        match url {
            None => whole.to_owned(),
            Some(url) => {
                let fragment = hash_at.map_or("", |at| &target[at..]);
                format!("[{text}]({url}{fragment}{title})")
            }
        }
    };

    let rewrite_outside_inline_code = move |line: &str, cwd: &str| -> String {
        let rewrite = |part: &str| {
            LINK.replace_all(part, |captures: &Captures<'_>| rewrite_link(captures, cwd))
                .into_owned()
        };
        let mut out = String::with_capacity(line.len());
        let mut last = 0;
        for code in INLINE_CODE.find_iter(line) {
            out.push_str(&rewrite(&line[last..code.start()]));
            out.push_str(code.as_str());
            last = code.end();
        }
        out.push_str(&rewrite(&line[last..]));
        out
    };

    Rc::new(move |markdown, _context| {
        if !markdown.contains("](") || !hyperlinks() {
            return Ok(Some(markdown.to_owned()));
        }
        let cwd = get_cwd();
        let mut fence: Option<String> = None;
        let lines: Vec<String> = markdown
            .split('\n')
            .map(|line| {
                let marker = FENCE
                    .captures(line)
                    .and_then(|captures| captures.get(1))
                    .map(|marker| marker.as_str());
                if let Some(open) = &fence {
                    if let Some(marker) = marker
                        && marker.as_bytes()[0] == open.as_bytes()[0]
                        && marker.len() >= open.len()
                    {
                        fence = None;
                    }
                    return line.to_owned();
                }
                if let Some(marker) = marker {
                    fence = Some(marker.to_owned());
                    return line.to_owned();
                }
                if INDENTED_CODE.is_match(line) {
                    line.to_owned()
                } else {
                    rewrite_outside_inline_code(line, &cwd)
                }
            })
            .collect();
        Ok(Some(lines.join("\n")))
    })
}

#[cfg(test)]
mod tests {
    //! senpi test/suite/regressions/issue-3415-message-path-links.test.ts.
    use std::cell::Cell;
    use std::path::Path;

    use super::*;
    use crate::components::markdown_transform::{MarkdownTransformContext, MessageType};

    const CONTEXT: MarkdownTransformContext = MarkdownTransformContext {
        message_type: MessageType::Assistant,
        is_streaming: false,
        available_width: 80,
    };

    fn repository() -> (tempfile::TempDir, String, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("realpath");
        std::fs::create_dir(root.join(".git")).expect("git");
        let cwd = root.join("packages").join("app");
        std::fs::create_dir_all(&cwd).expect("cwd");
        std::fs::create_dir(root.join("docs")).expect("docs");
        std::fs::write(root.join("docs").join("plan.md"), "plan").expect("plan");
        let root = root.to_string_lossy().into_owned();
        let cwd = cwd.to_string_lossy().into_owned();
        (dir, root, cwd)
    }

    fn plan_url(root: &str) -> String {
        path_to_file_url(
            &Path::new(root)
                .join("docs")
                .join("plan.md")
                .to_string_lossy(),
        )
    }

    fn transformer(
        cwd: &str,
        hyperlinks: bool,
        resolve: Option<MessagePathResolver>,
    ) -> MarkdownTransformer {
        let cwd = cwd.to_owned();
        create_message_path_link_transformer(
            Rc::new(move || cwd.clone()),
            MessagePathLinkOptions {
                hyperlinks: Some(Rc::new(move || hyperlinks)),
                resolve,
            },
        )
    }

    fn run(transform: &MarkdownTransformer, markdown: &str) -> String {
        transform(markdown, CONTEXT)
            .expect("transform")
            .expect("output")
    }

    fn counting_not_found() -> (Rc<Cell<usize>>, MessagePathResolver) {
        let calls = Rc::new(Cell::new(0));
        let counter = calls.clone();
        let resolve: MessagePathResolver = Rc::new(move |_, _, _| {
            counter.set(counter.get() + 1);
            ResolvedMessagePath::NotFound { tried: Vec::new() }
        });
        (calls, resolve)
    }

    #[test]
    fn points_a_relative_link_at_the_file_the_project_root_holds() {
        let (_dir, root, cwd) = repository();
        let transform = transformer(&cwd, true, None);
        assert_eq!(
            run(&transform, "See [the plan](docs/plan.md#goals)."),
            format!("See [the plan]({}#goals).", plan_url(&root))
        );
    }

    #[test]
    fn rewrites_the_other_links_when_one_link_is_badly_encoded() {
        let (_dir, root, cwd) = repository();
        let transform = transformer(&cwd, true, None);
        assert_eq!(
            run(&transform, "[bad](100%) [plan](docs/plan.md)"),
            format!("[bad](100%) [plan]({})", plan_url(&root))
        );
    }

    #[test]
    fn checks_a_missing_path_once_not_on_every_render() {
        let (_dir, _root, cwd) = repository();
        let (calls, resolve) = counting_not_found();
        let transform = transformer(&cwd, true, Some(resolve));
        for _ in 0..5 {
            run(&transform, "[gone](docs/missing.md)");
        }
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn leaves_links_alone_when_the_terminal_cannot_show_hyperlinks() {
        let (_dir, _root, cwd) = repository();
        let transform = transformer(&cwd, false, None);
        assert_eq!(
            run(&transform, "See [the plan](docs/plan.md)."),
            "See [the plan](docs/plan.md)."
        );
    }

    #[test]
    fn skips_indented_code_mismatched_fences_and_images_and_keeps_link_titles() {
        let (_dir, root, cwd) = repository();
        let transform = transformer(&cwd, true, None);
        let url = plan_url(&root);
        let markdown = [
            "    [indented](docs/plan.md)",
            "~~~",
            "```",
            "[inside](docs/plan.md)",
            "~~~",
            "[after](docs/plan.md)",
            "![img](docs/plan.md)",
            r#"[titled](docs/plan.md "The plan")"#,
        ]
        .join("\n");
        let expected = [
            "    [indented](docs/plan.md)".to_owned(),
            "~~~".to_owned(),
            "```".to_owned(),
            "[inside](docs/plan.md)".to_owned(),
            "~~~".to_owned(),
            format!("[after]({url})"),
            "![img](docs/plan.md)".to_owned(),
            format!(r#"[titled]({url} "The plan")"#),
        ];
        assert_eq!(
            run(&transform, &markdown).split('\n').collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn never_touches_the_file_system_for_a_network_path() {
        let (calls, resolve) = counting_not_found();
        let transform = transformer("/work", true, Some(resolve));
        run(
            &transform,
            r"[x](//attacker/share/x.pdf) [y](\\attacker\share\y.pdf)",
        );
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn leaves_urls_anchors_missing_files_and_code_alone() {
        let (_dir, _root, cwd) = repository();
        let transform = transformer(&cwd, true, None);
        let markdown = [
            "[site](https://example.com) [top](#intro) [gone](docs/missing.md)",
            "`[code](docs/plan.md)`",
            "```",
            "[fenced](docs/plan.md)",
            "```",
        ]
        .join("\n");
        assert_eq!(run(&transform, &markdown), markdown);
    }

    #[test]
    fn decode_uri_keeps_reserved_escapes_and_rejects_malformed_ones() {
        assert_eq!(decode_uri("a%20b%2Fc").as_deref(), Some("a b%2Fc"));
        assert_eq!(decode_uri("%C3%A9").as_deref(), Some("\u{e9}"));
        assert_eq!(decode_uri("100%"), None);
        assert_eq!(decode_uri("%C3"), None);
    }
}
