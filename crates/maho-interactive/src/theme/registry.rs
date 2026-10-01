use super::{ColorMode, Theme, theme_json::ThemeError};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
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
    registered: BTreeMap<String, Theme>,
    watched_name: Option<String>,
    reload_at: Option<u64>,
}
impl ThemeRegistry {
    pub fn new(custom_directory: PathBuf, name: &str, mode: ColorMode) -> Result<Self, ThemeError> {
        let theme = Theme::builtin("dark", mode)?;
        let mut registry = Self {
            current: theme,
            current_name: "dark".into(),
            custom_directory,
            registered: BTreeMap::new(),
            watched_name: None,
            reload_at: None,
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
    pub fn load_theme(&self, name: &str) -> Result<Theme, ThemeError> {
        if let Some(theme) = self.registered.get(name) {
            return Ok(theme.clone());
        }
        if matches!(name, "dark" | "light" | "grok-day" | "grok-night") {
            Theme::builtin(name, self.current.get_color_mode())
        } else {
            Theme::load_from_path(
                &self.custom_directory.join(format!("{name}.json")),
                self.current.get_color_mode(),
            )
        }
    }
    pub fn get_available_themes_with_paths(&self) -> Vec<ThemeInfo> {
        let mut themes: BTreeMap<String, Option<PathBuf>> =
            ["dark", "light", "grok-day", "grok-night"]
                .into_iter()
                .map(|n| (n.into(), None))
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
        themes
            .into_iter()
            .map(|(name, path)| ThemeInfo { name, path })
            .collect()
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
        }
    }
    pub fn stop_theme_watcher(&mut self) {
        self.watched_name = None;
        self.reload_at = None;
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
