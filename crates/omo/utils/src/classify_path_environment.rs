//! Heuristic classification of sync/network-backed paths that may not honour fsync.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathClassification {
    Icloud,
    Onedrive,
    DesktopSync,
    NetworkDrive,
    Unknown,
}

fn is_under(path: &str, parent: &str) -> bool {
    path == parent
        || path
            .strip_prefix(parent)
            .is_some_and(|rest| rest.starts_with('/'))
}

pub fn classify_path_environment(absolute_path: &str) -> PathClassification {
    if absolute_path.is_empty() {
        return PathClassification::Unknown;
    }
    let normalized = absolute_path.replace('\\', "/");
    if normalized.to_lowercase().contains("/onedrive") {
        return PathClassification::Onedrive;
    }
    if normalized.contains("/Library/Mobile Documents/") {
        return PathClassification::Icloud;
    }
    if is_under(&normalized, "/Volumes") {
        return PathClassification::NetworkDrive;
    }
    let sync_folder = |p: &str| {
        p.contains("/Desktop/")
            || p.ends_with("/Desktop")
            || p.contains("/Documents/")
            || p.ends_with("/Documents")
    };
    if normalized.starts_with("/Users/") && sync_folder(&normalized) {
        return PathClassification::DesktopSync;
    }
    let home = dirs::home_dir()
        .map(|home| home.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    if !home.is_empty()
        && (is_under(&normalized, &format!("{home}/Desktop"))
            || is_under(&normalized, &format!("{home}/Documents")))
    {
        return PathClassification::DesktopSync;
    }
    PathClassification::Unknown
}

pub fn describe_path_classification(classification: PathClassification) -> &'static str {
    match classification {
        PathClassification::Icloud => "iCloud Drive",
        PathClassification::Onedrive => "OneDrive",
        PathClassification::DesktopSync => "Desktop sync (macOS)",
        PathClassification::NetworkDrive => "Network drive",
        PathClassification::Unknown => "filesystem that does not support fsync",
    }
}
