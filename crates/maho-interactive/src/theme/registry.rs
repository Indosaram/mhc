use super::{ColorMode, Theme, theme_json::ThemeError};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::SystemTime,
};
#[derive(Debug, Clone)]
pub struct ThemeInfo {
    pub name: String,
    pub path: Option<PathBuf>,
}
pub struct ThemeRegistry {
    pub current: Theme,
    pub current_name: String,
    custom_directory: PathBuf,
    /// senpi `getThemesDir()`: the shipped builtin theme directory reported as builtin paths.
    builtin_directory: Option<PathBuf>,
    registered: BTreeMap<String, Theme>,
    watched_name: Option<String>,
    reload_at: Option<u64>,
    /// senpi `themeWatcher`: the OS watch on the custom themes directory and the file names it
    /// reported, drained by [`ThemeRegistry::poll_theme_watcher`].
    watcher: Option<(notify::RecommendedWatcher, std::sync::mpsc::Receiver<Option<PathBuf>>)>,
    /// Directory listing plus parsed custom theme names. Reused while the directory mtime,
    /// builtin directory, and registered sources are unchanged. Dropped when the watcher reports
    /// a change and when a debounced reload runs, so an in-place json edit cannot keep a stale name.
    theme_list_cache: RefCell<Option<ThemeListCache>>,
}

#[derive(Clone, PartialEq, Eq)]
struct ThemeListCacheKey {
    dir_mtime: Option<SystemTime>,
    builtin_directory: Option<PathBuf>,
    registered: Vec<(String, Option<PathBuf>)>,
}

struct ThemeListCache {
    key: ThemeListCacheKey,
    themes: Vec<ThemeInfo>,
}

impl ThemeRegistry {
    pub fn new(custom_directory: PathBuf, name: &str, mode: ColorMode) -> Result<Self, ThemeError> {
        let theme = Theme::builtin("dark", mode)?;
        let mut registry = Self {
            current: theme,
            current_name: "dark".into(),
            custom_directory,
            builtin_directory: None,
            registered: BTreeMap::new(),
            watched_name: None,
            reload_at: None,
            watcher: None,
            theme_list_cache: RefCell::new(None),
        };
        registry.init_theme(name, false)?;
        Ok(registry)
    }
    pub fn set_registered_themes(&mut self, themes: Vec<Theme>) -> Result<(), ThemeError> {
        self.registered.clear();
        for theme in themes {
            if theme.name.contains('/') {
                return Err(ThemeError::Invalid(format!(
                    "Invalid theme name {}",
                    theme.name
                )));
            }
            self.registered.insert(theme.name.clone(), theme);
        }
        Ok(())
    }
    pub fn set_builtin_directory(&mut self, directory: Option<PathBuf>) {
        self.builtin_directory = directory;
    }
    pub fn load_theme(&self, name: &str) -> Result<Theme, ThemeError> {
        if let Some(theme) = self.registered.get(name) {
            return Ok(theme.clone());
        }
        if matches!(name, "dark" | "light" | "grok-day" | "grok-night") {
            Theme::builtin(name, self.current.get_color_mode())
        } else {
            let path = self.custom_directory.join(format!("{name}.json"));
            // senpi `loadThemeJson`: a missing custom file is `Theme not found: <name>`.
            if !path.exists() {
                return Err(ThemeError::Schema(format!("Theme not found: {name}")));
            }
            Theme::load_from_path(&path, self.current.get_color_mode())
        }
    }
    pub fn get_available_themes_with_paths(&self) -> Vec<ThemeInfo> {
        let key = self.theme_list_cache_key();
        if let Some(themes) = self
            .theme_list_cache
            .borrow()
            .as_ref()
            .filter(|cached| cached.key == key)
            .map(|cached| cached.themes.clone())
        {
            return themes;
        }
        let themes = self.scan_available_themes();
        *self.theme_list_cache.borrow_mut() = Some(ThemeListCache {
            key,
            themes: themes.clone(),
        });
        themes
    }

    fn theme_list_cache_key(&self) -> ThemeListCacheKey {
        ThemeListCacheKey {
            dir_mtime: std::fs::metadata(&self.custom_directory)
                .and_then(|metadata| metadata.modified())
                .ok(),
            builtin_directory: self.builtin_directory.clone(),
            registered: self
                .registered
                .iter()
                .map(|(name, theme)| (name.clone(), theme.source_path.clone()))
                .collect(),
        }
    }

    fn scan_available_themes(&self) -> Vec<ThemeInfo> {
        let mut themes: BTreeMap<String, Option<PathBuf>> =
            ["dark", "light", "grok-day", "grok-night"]
                .into_iter()
                .map(|n| {
                    let path = self
                        .builtin_directory
                        .as_ref()
                        .map(|directory| directory.join(format!("{n}.json")));
                    (n.into(), path)
                })
                .collect();
        if let Ok(entries) = std::fs::read_dir(&self.custom_directory) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "json")
                    && let Ok(theme) = Theme::load_from_path(&path, self.current.get_color_mode())
                {
                    themes.entry(theme.name).or_insert(Some(path));
                }
            }
        }
        for (name, theme) in &self.registered {
            themes
                .entry(name.clone())
                .or_insert(theme.source_path.clone());
        }
        let mut themes: Vec<ThemeInfo> = themes
            .into_iter()
            .map(|(name, path)| ThemeInfo { name, path })
            .collect();
        // senpi `a.name.localeCompare(b.name)`: case-insensitive, lowercase first on ties.
        themes.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| b.name.cmp(&a.name))
        });
        themes
    }
    pub fn init_theme(&mut self, name: &str, watch: bool) -> Result<(), ThemeError> {
        match self.set_theme(name, watch) {
            Ok(()) => Ok(()),
            Err(_) => {
                self.current = Theme::builtin("dark", self.current.get_color_mode())?;
                self.current_name = "dark".into();
                Ok(())
            }
        }
    }
    pub fn set_theme(&mut self, name: &str, watch: bool) -> Result<(), ThemeError> {
        self.current_name = name.into();
        match self.load_theme(name) {
            Ok(theme) => {
                self.current = theme;
                if watch {
                    self.start_theme_watcher();
                }
                Ok(())
            }
            Err(error) => {
                self.current = Theme::builtin("dark", self.current.get_color_mode())?;
                self.current_name = "dark".into();
                Err(error)
            }
        }
    }
    pub fn set_theme_instance(&mut self, theme: Theme) {
        self.current = theme;
        self.current_name = "<in-memory>".into();
        self.stop_theme_watcher();
    }
    pub fn start_theme_watcher(&mut self) {
        self.stop_theme_watcher();
        if matches!(self.current_name.as_str(), "dark" | "light") {
            return;
        }
        if self
            .custom_directory
            .join(format!("{}.json", self.current_name))
            .exists()
        {
            self.watched_name = Some(self.current_name.clone());
            let (sender, receiver) = std::sync::mpsc::channel();
            let handler = move |event: notify::Result<notify::Event>| {
                let Ok(event) = event else { return };
                if !matches!(
                    event.kind,
                    notify::EventKind::Create(_)
                        | notify::EventKind::Modify(_)
                        | notify::EventKind::Remove(_)
                ) {
                    return;
                }
                if event.paths.is_empty() {
                    let _ = sender.send(None);
                }
                for path in event.paths {
                    let _ = sender.send(path.file_name().map(PathBuf::from));
                }
            };
            // A directory that cannot be watched leaves hot reload off, as senpi's
            // `watchWithErrorHandler` does.
            if let Ok(mut watcher) = notify::recommended_watcher(handler)
                && notify::Watcher::watch(
                    &mut watcher,
                    &self.custom_directory,
                    notify::RecursiveMode::NonRecursive,
                )
                .is_ok()
            {
                self.watcher = Some((watcher, receiver));
            }
        }
    }
    pub fn stop_theme_watcher(&mut self) {
        self.watched_name = None;
        self.reload_at = None;
        self.watcher = None;
    }
    /// Feeds the file names the OS watcher reported into [`ThemeRegistry::on_file_change`], then
    /// runs the debounced reload; `true` when the current theme was reloaded (senpi's
    /// `onThemeChangeCallback`).
    pub fn poll_theme_watcher(&mut self, now: u64) -> bool {
        let changes: Vec<Option<PathBuf>> = self
            .watcher
            .as_ref()
            .map(|(_, receiver)| receiver.try_iter().collect())
            .unwrap_or_default();
        // Any reported path, not only the watched theme file: the settings list parses every json.
        if !changes.is_empty() {
            self.invalidate_theme_list_cache();
        }
        for change in changes {
            self.on_file_change(change.as_deref(), now);
        }
        self.reload_due(now)
    }

    fn invalidate_theme_list_cache(&self) {
        *self.theme_list_cache.borrow_mut() = None;
    }

    pub fn on_file_change(&mut self, filename: Option<&Path>, now: u64) {
        let Some(name) = self.watched_name.as_deref() else {
            return;
        };
        let target = format!("{name}.json");
        if filename.is_none_or(|file| file == Path::new(&target)) {
            self.reload_at = Some(now.saturating_add(100));
        }
    }
    pub fn reload_due(&mut self, now: u64) -> bool {
        if self.reload_at.is_none_or(|due| now < due) {
            return false;
        }
        self.reload_at = None;
        // The debounced reload is the other path that observes an in-place edit. Drop the list
        // even when the watched name no longer matches, so a failed or skipped apply cannot
        // leave the parsed name cached.
        self.invalidate_theme_list_cache();
        let Some(name) = self
            .watched_name
            .as_ref()
            .filter(|n| *n == &self.current_name)
        else {
            return false;
        };
        let path = self.custom_directory.join(format!("{name}.json"));
        match Theme::load_from_path(&path, self.current.get_color_mode()) {
            Ok(theme) => {
                self.registered.insert(name.clone(), theme.clone());
                self.current = theme;
                true
            }
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On a8ac6196 `get_available_themes_with_paths` `read_dir`s and parses every custom theme
    /// json on each call, so rewriting a theme file changes the returned name before this function
    /// returns. The cache must keep the previous list until the directory mtime changes.
    /// Directory mtime is restored and bumped with `set_modified` so the test does not depend on
    /// whether the filesystem bumps a directory when a child file is rewritten.
    #[test]
    fn theme_list_cache_ignores_json_rewrites_until_directory_mtime_changes() {
        let dir = tempfile::tempdir().expect("temp themes dir");
        let mut document: serde_json::Value =
            serde_json::from_str(include_str!("dark.json")).expect("dark theme json");
        document["name"] = serde_json::json!("alpha");
        let path = dir.path().join("alpha.json");
        std::fs::write(&path, serde_json::to_string(&document).expect("theme json"))
            .expect("write theme");
        let before = std::fs::metadata(dir.path())
            .expect("dir metadata")
            .modified()
            .expect("dir mtime");
        let registry = ThemeRegistry::new(
            dir.path().to_path_buf(),
            "dark",
            crate::theme::ColorMode::Truecolor,
        )
        .expect("theme registry");
        let first = registry.get_available_themes_with_paths();
        assert!(first.iter().any(|theme| theme.name == "alpha"), "{first:?}");

        document["name"] = serde_json::json!("beta");
        std::fs::write(&path, serde_json::to_string(&document).expect("theme json"))
            .expect("rewrite theme");
        set_dir_mtime(dir.path(), before);
        let second = registry.get_available_themes_with_paths();
        assert!(
            second.iter().any(|theme| theme.name == "alpha"),
            "cached theme list was re-parsed on the UI thread: {second:?}"
        );
        assert!(second.iter().all(|theme| theme.name != "beta"), "{second:?}");

        let bumped = before
            .checked_add(std::time::Duration::from_secs(1))
            .expect("directory mtime bump");
        set_dir_mtime(dir.path(), bumped);
        let third = registry.get_available_themes_with_paths();
        assert!(third.iter().any(|theme| theme.name == "beta"), "{third:?}");
        assert!(third.iter().all(|theme| theme.name != "alpha"), "{third:?}");
    }

    /// On a8ac6196 the second `get_available_themes_with_paths` has already parsed `beta` (there is
    /// no list cache), so the name is not still `alpha` when `poll_theme_watcher` is called.
    #[test]
    fn watcher_reported_change_invalidates_theme_list_cache() {
        let dir = tempfile::tempdir().expect("temp themes dir");
        let mut document: serde_json::Value =
            serde_json::from_str(include_str!("dark.json")).expect("dark theme json");
        document["name"] = serde_json::json!("alpha");
        let path = dir.path().join("alpha.json");
        std::fs::write(&path, serde_json::to_string(&document).expect("theme json"))
            .expect("write theme");
        let before = std::fs::metadata(dir.path())
            .expect("dir metadata")
            .modified()
            .expect("dir mtime");
        let mut registry = ThemeRegistry::new(
            dir.path().to_path_buf(),
            "dark",
            crate::theme::ColorMode::Truecolor,
        )
        .expect("theme registry");
        assert!(
            registry
                .get_available_themes_with_paths()
                .iter()
                .any(|theme| theme.name == "alpha")
        );

        document["name"] = serde_json::json!("beta");
        std::fs::write(&path, serde_json::to_string(&document).expect("theme json"))
            .expect("rewrite theme");
        set_dir_mtime(dir.path(), before);
        let cached = registry.get_available_themes_with_paths();
        assert!(
            cached.iter().any(|theme| theme.name == "alpha"),
            "a8ac6196 already re-parsed the rewritten json before any watcher poll: {cached:?}"
        );
        assert!(cached.iter().all(|theme| theme.name != "beta"), "{cached:?}");

        let (sender, receiver) = std::sync::mpsc::channel();
        let watcher = notify::recommended_watcher(|_event: notify::Result<notify::Event>| {})
            .expect("theme watcher");
        registry.watcher = Some((watcher, receiver));
        sender
            .send(Some(std::path::PathBuf::from("alpha.json")))
            .expect("report theme change");
        // now=0 and watched_name is unset, so this must not take the debounced reload path.
        assert!(!registry.poll_theme_watcher(0));
        let refreshed = registry.get_available_themes_with_paths();
        assert!(
            refreshed.iter().any(|theme| theme.name == "beta"),
            "watcher-reported change left the cached theme name: {refreshed:?}"
        );
        assert!(refreshed.iter().all(|theme| theme.name != "alpha"), "{refreshed:?}");
    }

    /// On a8ac6196 the list read after the rewrite has already parsed `beta`, before `reload_due`.
    /// The registered entry is seeded first so a successful reload does not change the cache key
    /// (same name and source path); only dropping the cache can surface `beta`.
    #[test]
    fn reload_due_invalidates_theme_list_cache() {
        let dir = tempfile::tempdir().expect("temp themes dir");
        let mut document: serde_json::Value =
            serde_json::from_str(include_str!("dark.json")).expect("dark theme json");
        document["name"] = serde_json::json!("alpha");
        let path = dir.path().join("alpha.json");
        std::fs::write(&path, serde_json::to_string(&document).expect("theme json"))
            .expect("write theme");
        let before = std::fs::metadata(dir.path())
            .expect("dir metadata")
            .modified()
            .expect("dir mtime");
        let mut registry = ThemeRegistry::new(
            dir.path().to_path_buf(),
            "dark",
            crate::theme::ColorMode::Truecolor,
        )
        .expect("theme registry");
        let loaded = registry.load_theme("alpha").expect("load alpha");
        registry.registered.insert("alpha".into(), loaded);
        assert!(
            registry
                .get_available_themes_with_paths()
                .iter()
                .any(|theme| theme.name == "alpha")
        );

        document["name"] = serde_json::json!("beta");
        std::fs::write(&path, serde_json::to_string(&document).expect("theme json"))
            .expect("rewrite theme");
        set_dir_mtime(dir.path(), before);
        let cached = registry.get_available_themes_with_paths();
        assert!(
            cached.iter().all(|theme| theme.name != "beta"),
            "a8ac6196 already re-parsed the rewritten json before reload_due: {cached:?}"
        );

        registry.current_name = "alpha".into();
        registry.watched_name = Some("alpha".into());
        registry.on_file_change(Some(std::path::Path::new("alpha.json")), 0);
        assert!(registry.reload_due(100));
        let refreshed = registry.get_available_themes_with_paths();
        assert!(
            refreshed.iter().any(|theme| theme.name == "beta"),
            "debounced reload left the cached theme name: {refreshed:?}"
        );
    }

    /// `File::open` on a directory is Access Denied on Windows, so ask for attribute access
    /// with `FILE_FLAG_BACKUP_SEMANTICS` (same helper shape as maho-ext-hooks' trust storage).
    #[cfg(windows)]
    fn open_dir_for_times(dir: &std::path::Path) -> std::io::Result<std::fs::File> {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
        const FILE_READ_ATTRIBUTES: u32 = 0x0080;
        const FILE_SHARE_READ: u32 = 1;
        const FILE_SHARE_WRITE: u32 = 2;
        const FILE_SHARE_DELETE: u32 = 4;
        std::fs::OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES | FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(dir)
    }
    #[cfg(not(windows))]
    fn open_dir_for_times(dir: &std::path::Path) -> std::io::Result<std::fs::File> {
        std::fs::File::open(dir)
    }

    fn set_dir_mtime(dir: &std::path::Path, mtime: std::time::SystemTime) {
        open_dir_for_times(dir)
            .expect("open themes dir")
            .set_modified(mtime)
            .expect("set directory mtime");
        let restored = std::fs::metadata(dir)
            .expect("dir metadata")
            .modified()
            .expect("dir mtime");
        assert_eq!(mtime, restored, "directory mtime must stick so the cache key is deterministic");
    }
}
