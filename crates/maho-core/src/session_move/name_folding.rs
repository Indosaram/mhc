//! Port of senpi packages/coding-agent/src/core/session-move/name-folding.ts.
//!
//! Names that are one name on some file systems (senpi#6472): two paths that differ only in letter
//! case, or only in Unicode form (a composed and a decomposed spelling of one character).

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, SystemTime};

use unicode_normalization::UnicodeNormalization;

use super::bundle::{MoveBundleInvalidError, shown};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NameFolding {
    pub letter_case: bool,
    pub unicode_form: bool,
}

pub type ProbeFoldingFn<'a> = &'a (
        dyn Fn(&Path) -> Pin<Box<dyn Future<Output = Result<NameFolding, String>> + Send>>
            + Send
            + Sync
    );

const EVERY_FOLDING: NameFolding = NameFolding {
    letter_case: true,
    unicode_form: true,
};

const STALE_PROBE_AGE: Duration = Duration::from_secs(60 * 60);

async fn exists(path: &Path) -> bool {
    tokio::fs::try_exists(path).await.unwrap_or(false)
}

async fn nearest_folder(path: &Path) -> PathBuf {
    let mut cur = path.to_path_buf();
    loop {
        if let Ok(meta) = tokio::fs::metadata(&cur).await
            && meta.is_dir()
        {
            return cur;
        }
        let Some(parent) = cur.parent().map(Path::to_path_buf) else {
            return cur;
        };
        if parent == cur {
            return cur;
        }
        cur = parent;
    }
}

fn is_probe_file_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(".name-probe-") else {
        return false;
    };
    let Some((hex_part, _)) = rest.split_once('-') else {
        return false;
    };
    hex_part.len() == 12
        && hex_part
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

async fn sweep_stale_probes(folder: &Path) {
    let Ok(mut rd) = tokio::fs::read_dir(folder).await else {
        return;
    };
    let now = SystemTime::now();
    while let Ok(Some(entry)) = rd.next_entry().await {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !is_probe_file_name(name_str) {
            continue;
        }
        let file = entry.path();
        if let Ok(meta) = tokio::fs::metadata(&file).await
            && let Ok(modified) = meta.modified()
            && let Ok(age) = now.duration_since(modified)
            && age > STALE_PROBE_AGE
        {
            let _ = tokio::fs::remove_file(&file).await;
        }
    }
}

pub async fn probe_name_folding(path: &Path) -> Result<NameFolding, String> {
    let folder = nearest_folder(path).await;
    sweep_stale_probes(&folder).await;
    let uuid_hex: String = uuid::Uuid::new_v4()
        .as_simple()
        .to_string()
        .chars()
        .take(12)
        .collect();
    let tag = format!(".name-probe-{uuid_hex}-");
    let written = folder.join(format!("{tag}A\u{00e9}"));
    let mut opts = tokio::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    opts.open(&written).await.map_err(|e| e.to_string())?;
    let letter_case = exists(&folder.join(format!("{tag}a\u{00e9}"))).await;
    let unicode_form = exists(&folder.join(format!("{tag}Ae\u{0301}"))).await;
    let _ = tokio::fs::remove_file(&written).await;
    Ok(NameFolding {
        letter_case,
        unicode_form,
    })
}

fn segments_joined(path: &str) -> String {
    path.split(['/', '\\']).collect::<Vec<_>>().join("/")
}

pub fn one_name_under(paths: &[String], folding: NameFolding) -> Option<String> {
    let spelled = |path: &str| -> String {
        let cased = if folding.letter_case {
            path.to_lowercase()
        } else {
            path.to_owned()
        };
        if folding.unicode_form {
            cased.nfc().collect()
        } else {
            cased
        }
    };
    let mut by_name: HashMap<String, &str> = HashMap::new();
    for path in paths {
        let joined = segments_joined(path);
        let name = spelled(&joined);
        if let Some(&other) = by_name.get(&name)
            && other != path.as_str()
        {
            return Some(format!("{} and {} are one name", shown(path), shown(other)));
        }
        by_name.insert(name, path.as_str());
    }
    for (name, path) in &by_name {
        let path_joined = segments_joined(path);
        let parts: Vec<&str> = name.split('/').collect();
        let mut folder = String::new();
        for segment in parts.iter().take(parts.len().saturating_sub(1)) {
            if folder.is_empty() {
                folder.push_str(segment);
            } else {
                folder.push('/');
                folder.push_str(segment);
            }
            if let Some(&file) = by_name.get(&folder) {
                let file_prefix = format!("{}/", segments_joined(file));
                if !path_joined.starts_with(&file_prefix) {
                    return Some(format!(
                        "{} is a file where {} needs a folder",
                        shown(file),
                        shown(path)
                    ));
                }
            }
        }
    }
    None
}

pub async fn refuse_names_that_fold(
    layouts: &[&[String]],
    folder: &Path,
    probe: Option<ProbeFoldingFn<'_>>,
) -> Result<(), MoveBundleInvalidError> {
    if layouts
        .iter()
        .all(|names| one_name_under(names, EVERY_FOLDING).is_none())
    {
        return Ok(());
    }
    let folding = match probe {
        Some(f) => f(folder).await.map_err(MoveBundleInvalidError::new)?,
        None => probe_name_folding(folder)
            .await
            .map_err(MoveBundleInvalidError::new)?,
    };
    for names in layouts {
        if let Some(clash) = one_name_under(names, folding) {
            return Err(MoveBundleInvalidError::new(format!(
                "{clash} on the file system this host would write the bundle to"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_case_and_unicode_folding_clashes() {
        let names = vec!["files/Notes.txt".to_owned(), "files/notes.txt".to_owned()];
        assert!(
            one_name_under(
                &names,
                NameFolding {
                    letter_case: true,
                    unicode_form: false
                }
            )
            .is_some()
        );
        assert!(
            one_name_under(
                &names,
                NameFolding {
                    letter_case: false,
                    unicode_form: true
                }
            )
            .is_none()
        );

        let nfd_nfc = vec![
            "files/caf\u{00e9}.txt".to_owned(),
            "files/cafe\u{0301}.txt".to_owned(),
        ];
        assert!(
            one_name_under(
                &nfd_nfc,
                NameFolding {
                    letter_case: false,
                    unicode_form: true
                }
            )
            .is_some()
        );

        let file_as_dir = vec!["files/A".to_owned(), "files/a/sub.txt".to_owned()];
        assert!(one_name_under(&file_as_dir, EVERY_FOLDING).is_some());
    }
}
