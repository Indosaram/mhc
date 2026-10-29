//! The `side_panel` settings block (spec 1-2).

use serde_json::Value;
use std::fmt;

/// Section ids in display order (spec 4). `location` is not configurable.
pub const SECTION_KEYS: [&str; 8] = [
    "session", "goal", "context", "usage", "agents", "tools", "files", "memory",
];

/// Configured panel width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width {
    /// Fixed number of columns (24..=160).
    Columns(u32),
    /// Percent of the terminal width (10..=50).
    Percent(u32),
}

/// Which sections are enabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sections {
    pub session: bool,
    pub goal: bool,
    pub context: bool,
    pub usage: bool,
    pub agents: bool,
    pub tools: bool,
    pub files: bool,
    pub memory: bool,
}

impl Default for Sections {
    fn default() -> Self {
        Self {
            session: true,
            goal: true,
            context: true,
            usage: false,
            agents: true,
            tools: true,
            files: true,
            memory: true,
        }
    }
}

impl Sections {
    fn slot(&mut self, key: &str) -> Option<&mut bool> {
        Some(match key {
            "session" => &mut self.session,
            "goal" => &mut self.goal,
            "context" => &mut self.context,
            "usage" => &mut self.usage,
            "agents" => &mut self.agents,
            "tools" => &mut self.tools,
            "files" => &mut self.files,
            "memory" => &mut self.memory,
            _ => return None,
        })
    }
}

/// Validated settings with defaults applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    pub width: Width,
    pub min_columns: u32,
    pub clickable: bool,
    pub usage_poll_seconds: u32,
    pub sections: Sections,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            width: Width::Percent(26),
            min_columns: 120,
            clickable: true,
            usage_poll_seconds: 150,
            sections: Sections::default(),
        }
    }
}

/// A validation error naming the offending field (`side_panel.width`, ...).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsError {
    pub field: String,
    pub message: String,
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

impl std::error::Error for SettingsError {}

fn error(field: &str, message: impl Into<String>) -> SettingsError {
    SettingsError {
        field: format!("side_panel.{field}"),
        message: message.into(),
    }
}

fn boolean(value: &Value, field: &str) -> Result<bool, SettingsError> {
    value
        .as_bool()
        .ok_or_else(|| error(field, "must be true or false"))
}

fn int_in(value: &Value, field: &str, min: u32, max: u32) -> Result<u32, SettingsError> {
    let message = format!("must be a whole number from {min} to {max}");
    let number = value
        .as_u64()
        .ok_or_else(|| error(field, message.clone()))?;
    u32::try_from(number)
        .ok()
        .filter(|n| (min..=max).contains(n))
        .ok_or_else(|| error(field, message))
}

/// Parses a width value: an integer 24..=160 or `"NN%"` with exactly two digits, 10..=50.
pub fn parse_width(value: &Value) -> Result<Width, SettingsError> {
    const MESSAGE: &str =
        "must be a column count from 24 to 160 or a percentage \"10%\" to \"50%\"";
    if value.is_number() {
        return int_in(value, "width", 24, 160)
            .map(Width::Columns)
            .map_err(|_| error("width", MESSAGE));
    }
    let text = value.as_str().ok_or_else(|| error("width", MESSAGE))?;
    let digits = text
        .strip_suffix('%')
        .ok_or_else(|| error("width", MESSAGE))?;
    if digits.len() != 2 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(error("width", MESSAGE));
    }
    let pct: u32 = digits.parse().map_err(|_| error("width", MESSAGE))?;
    if !(10..=50).contains(&pct) {
        return Err(error("width", MESSAGE));
    }
    Ok(Width::Percent(pct))
}

impl Settings {
    /// Validates the `side_panel` value of the merged config (`None` = absent → defaults).
    pub fn from_value(value: Option<&Value>) -> Result<Self, SettingsError> {
        let mut settings = Self::default();
        let Some(value) = value else {
            return Ok(settings);
        };
        if value.is_null() {
            return Ok(settings);
        }
        let object = value.as_object().ok_or_else(|| SettingsError {
            field: "side_panel".into(),
            message: "must be an object".into(),
        })?;
        for (key, item) in object {
            match key.as_str() {
                "enabled" => settings.enabled = boolean(item, key)?,
                "width" => settings.width = parse_width(item)?,
                "min_columns" => settings.min_columns = int_in(item, key, 60, 400)?,
                "clickable" => settings.clickable = boolean(item, key)?,
                "usage_poll_seconds" => {
                    settings.usage_poll_seconds = int_in(item, key, 60, 3600)?;
                }
                "sections" => {
                    let sections = item
                        .as_object()
                        .ok_or_else(|| error("sections", "must be an object"))?;
                    for (name, flag) in sections {
                        let field = format!("sections.{name}");
                        let parsed = boolean(flag, &field)?;
                        let slot = settings
                            .sections
                            .slot(name)
                            .ok_or_else(|| error(&field, "is not a known section"))?;
                        *slot = parsed;
                    }
                }
                other => return Err(error(other, "is not a known setting")),
            }
        }
        Ok(settings)
    }
}

/// The enable decision (spec 1): the per-run flag wins over config in both directions.
pub fn panel_enabled(config_enabled: bool, flag: Option<bool>) -> bool {
    flag.unwrap_or(config_enabled)
}
