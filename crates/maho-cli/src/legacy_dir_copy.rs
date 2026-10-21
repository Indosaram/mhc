//! Port of senpi `legacy-dir-copy.ts`.
#[cfg(windows)]
use std::os::windows::fs::FileTypeExt;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

pub fn is_regenerable_entry(name: &str) -> bool {
    matches!(name, "cache" | "logs" | "tmp" | "migrations-state.json") || name.ends_with(".log")
}

pub fn paths_point_to_same_location(left: &Path, right: &Path) -> io::Result<bool> {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => Ok(left == right),
        (Err(error), _) | (_, Err(error)) if missing(&error) => Ok(false),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

fn missing(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

fn canonical_path(path: &Path) -> io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    let absolute = normalized;
    match fs::canonicalize(&absolute) {
        Ok(path) => Ok(path),
        Err(error) if missing(&error) => match (absolute.parent(), absolute.file_name()) {
            (Some(parent), Some(name)) => Ok(canonical_path(parent)?.join(name)),
            _ => Ok(absolute),
        },
        Err(error) => Err(error),
    }
}

pub fn is_within_or_same_path(child: &Path, parent: &Path) -> io::Result<bool> {
    Ok(canonical_path(child)?.starts_with(canonical_path(parent)?))
}

fn copy_entry(source: &Path, target: &Path) -> io::Result<()> {
    if fs::symlink_metadata(target).is_ok() {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(source)?;
    if metadata.is_symlink() {
        let link = fs::read_link(source)?;
        // cpSync defaults to verbatimSymlinks:false: resolve a relative link against its source.
        let link = if link.is_absolute() {
            link
        } else {
            std::path::absolute(source.parent().unwrap_or(Path::new(".")).join(link))?
        };
        #[cfg(unix)]
        std::os::unix::fs::symlink(link, target)?;
        #[cfg(windows)]
        if metadata.file_type().is_symlink_dir() {
            std::os::windows::fs::symlink_dir(link, target)?;
        } else {
            std::os::windows::fs::symlink_file(link, target)?;
        }
    } else if metadata.is_dir() {
        fs::create_dir(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_entry(&entry.path(), &target.join(entry.file_name()))?;
        }
        fs::set_permissions(target, metadata.permissions())?;
    } else {
        let mut input = fs::File::open(source)?;
        let mut output = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(target)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
            Err(error) => return Err(error),
        };
        io::copy(&mut input, &mut output)?;
        output.set_times(
            fs::FileTimes::new()
                .set_accessed(metadata.accessed()?)
                .set_modified(metadata.modified()?),
        )?;
        output.set_permissions(metadata.permissions())?;
    }
    Ok(())
}

pub fn copy_missing_entries(
    source: &Path,
    target: &Path,
    include: impl Fn(&str) -> bool,
) -> io::Result<Vec<String>> {
    let entries = match fs::read_dir(source) {
        Ok(entries) => entries,
        Err(error) if missing(&error) => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut copied = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let destination = target.join(entry.file_name());
        if !include(&name) || destination.exists() {
            continue;
        }
        let result =
            fs::create_dir_all(target).and_then(|()| copy_entry(&entry.path(), &destination));
        match result {
            Ok(()) => copied.push(name),
            Err(error) => eprintln!(
                "Could not copy {} → {}: {error}",
                entry.path().display(),
                destination.display()
            ),
        }
    }
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_preserves_file_times_and_existing_entries() {
        // Given
        let root = tempfile::tempdir().expect("tempdir");
        let source = root.path().join("source");
        let target = root.path().join("target");
        fs::create_dir_all(source.join("nested")).expect("source");
        fs::create_dir(&target).expect("target");
        fs::write(source.join("nested/config"), "legacy").expect("write");
        let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1234);
        fs::OpenOptions::new().write(true).open(source.join("nested/config"))
            .expect("open")
            .set_modified(time)
            .expect("mtime");
        fs::write(source.join("existing"), "old").expect("source");
        fs::write(target.join("existing"), "current").expect("target");
        // When
        copy_missing_entries(&source, &target, |_| true).expect("copy");
        // Then
        assert_eq!(
            fs::metadata(target.join("nested/config"))
                .expect("metadata")
                .modified()
                .expect("mtime"),
            time
        );
        assert_eq!(
            fs::read(target.join("existing")).expect("existing"),
            b"current"
        );
    }

    #[cfg(unix)]
    #[test]
    fn copy_keeps_links_and_canonical_ownership_resolves_missing_suffixes() {
        // Given
        let root = tempfile::tempdir().expect("tempdir");
        let source = root.path().join("source");
        let alias = root.path().join("alias");
        let target = root.path().join("target");
        fs::create_dir(&source).expect("source");
        std::os::unix::fs::symlink("missing", source.join("link")).expect("link");
        std::os::unix::fs::symlink(&source, &alias).expect("alias");
        // When
        copy_missing_entries(&source, &target, |_| true).expect("copy");
        // Then
        assert_eq!(
            fs::read_link(target.join("link")).expect("link"),
            source.join("missing")
        );
        assert!(
            is_within_or_same_path(&alias.join("not-created/child"), &source).expect("containment")
        );
        assert!(!is_within_or_same_path(&alias.join("../outside"), &source).expect("outside"));
    }
}
