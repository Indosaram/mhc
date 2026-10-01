//! Port of theme-controller.ts. Generation tokens reject stale background detection replies.
use super::theme::{
    ColorMode, TerminalTheme, Theme, parse_auto_theme_setting, resolve_theme_setting,
};
use super::theme_json::ThemeError;
pub struct InteractiveThemeController {
    pub terminal_theme: TerminalTheme,
    pub active_theme: Theme,
    pub current_theme_setting: Option<String>,
    pub auto_sync_enabled: bool,
    generation: u64,
}
impl InteractiveThemeController {
    pub fn new(
        setting: Option<String>,
        terminal_theme: TerminalTheme,
        mode: ColorMode,
    ) -> Result<Self, ThemeError> {
        let name = resolve_theme_setting(setting.as_deref(), terminal_theme).unwrap_or(
            match terminal_theme {
                TerminalTheme::Dark => "dark",
                TerminalTheme::Light => "light",
            },
        );
        let theme = Theme::builtin(name, mode).or_else(|_| Theme::builtin("dark", mode))?;
        Ok(Self {
            terminal_theme,
            active_theme: theme,
            current_theme_setting: setting,
            auto_sync_enabled: false,
            generation: 0,
        })
    }
    pub fn apply_from_settings(&mut self) -> Result<u64, ThemeError> {
        self.generation += 1;
        self.auto_sync_enabled =
            parse_auto_theme_setting(self.current_theme_setting.as_deref()).is_some();
        let name =
            resolve_theme_setting(self.current_theme_setting.as_deref(), self.terminal_theme)
                .unwrap_or(match self.terminal_theme {
                    TerminalTheme::Dark => "dark",
                    TerminalTheme::Light => "light",
                });
        self.active_theme = Theme::builtin(name, self.active_theme.get_color_mode())?;
        Ok(self.generation)
    }
    pub fn apply_terminal_theme(&mut self, theme: TerminalTheme) -> Result<bool, ThemeError> {
        if !self.auto_sync_enabled {
            return Ok(false);
        }
        self.terminal_theme = theme;
        let Some((light, dark)) = parse_auto_theme_setting(self.current_theme_setting.as_deref())
        else {
            self.auto_sync_enabled = false;
            return Ok(false);
        };
        let name = match theme {
            TerminalTheme::Light => light,
            TerminalTheme::Dark => dark,
        };
        if self.active_theme.name == name {
            return Ok(false);
        }
        self.active_theme = Theme::builtin(name, self.active_theme.get_color_mode())?;
        Ok(true)
    }
    pub fn resolve_detection(
        &mut self,
        generation: u64,
        theme: TerminalTheme,
    ) -> Result<bool, ThemeError> {
        if generation != self.generation {
            return Ok(false);
        }
        self.apply_terminal_theme(theme)
    }
    pub fn set_theme_name(&mut self, name: &str) -> Result<(), ThemeError> {
        self.generation += 1;
        self.auto_sync_enabled = false;
        match Theme::builtin(name, self.active_theme.get_color_mode()) {
            Ok(theme) => {
                self.active_theme = theme;
                self.current_theme_setting = Some(name.into());
                Ok(())
            }
            Err(error) => {
                self.active_theme = Theme::builtin("dark", self.active_theme.get_color_mode())?;
                Err(error)
            }
        }
    }
    pub fn set_theme_instance(&mut self, theme: Theme) {
        self.generation += 1;
        self.auto_sync_enabled = false;
        self.active_theme = theme;
    }
    pub fn dispose(&mut self) {
        self.generation += 1;
        self.auto_sync_enabled = false;
    }
}
