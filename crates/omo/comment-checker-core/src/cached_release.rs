//! Release cache version marker (port of `src/cached-release.ts`).

use std::fs;
use std::io;
use std::path::Path;

use crate::release::COMMENT_CHECKER_RELEASE_VERSION;

/// The shared binary cache has one unversioned slot, so a checker an older install left there would be
/// reused forever and the pinned release never fetched (#8850). The downloader records the release it
/// extracted in this file beside the binary; a missing or different marker means the slot is stale.
pub const COMMENT_CHECKER_VERSION_MARKER: &str = "comment-checker.version";

pub type MarkerReader<'a> = &'a dyn Fn(&Path) -> io::Result<String>;

pub fn is_cached_comment_checker_current(
    cache_dir: &Path,
    read_file: Option<MarkerReader<'_>>,
) -> bool {
    let marker_path = cache_dir.join(COMMENT_CHECKER_VERSION_MARKER);
    let content = match read_file {
        Some(reader) => reader(&marker_path).ok(),
        None => fs::read_to_string(&marker_path).ok(),
    };
    content.as_deref().map(str::trim) == Some(COMMENT_CHECKER_RELEASE_VERSION)
}

pub fn record_cached_comment_checker_release(cache_dir: &Path) -> io::Result<()> {
    fs::write(
        cache_dir.join(COMMENT_CHECKER_VERSION_MARKER),
        format!("{COMMENT_CHECKER_RELEASE_VERSION}
"),
    )
}
