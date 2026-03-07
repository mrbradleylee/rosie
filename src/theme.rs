use anyhow::{Result, anyhow};
use ratatui::style::Color;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_THEME: ThemeName = ThemeName::RosePine;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThemeName {
    Catppuccin,
    RosePine,
}

impl ThemeName {
    pub fn from_config(value: Option<&str>) -> Option<Self> {
        let raw = value?.trim().to_ascii_lowercase();
        match raw.as_str() {
            "catppuccin" | "catppuccin-mocha" | "mocha" => Some(Self::Catppuccin),
            "rose-pine" | "rosepine" | "rose_pine" => Some(Self::RosePine),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Catppuccin => "catppuccin",
            Self::RosePine => "rose-pine",
        }
    }

    pub fn palette(self) -> ThemePalette {
        match self {
            Self::Catppuccin => ThemePalette {
                base: Color::Rgb(30, 30, 46),
                surface: Color::Rgb(24, 24, 37),
                surface_alt: Color::Rgb(49, 50, 68),
                text: Color::Rgb(205, 214, 244),
                muted: Color::Rgb(166, 173, 200),
                accent: Color::Rgb(137, 180, 250),
                success: Color::Rgb(166, 227, 161),
                warn: Color::Rgb(249, 226, 175),
                error: Color::Rgb(243, 139, 168),
                border: Color::Rgb(69, 71, 90),
                border_active: Color::Rgb(180, 190, 254),
            },
            Self::RosePine => ThemePalette {
                base: Color::Rgb(25, 23, 36),
                surface: Color::Rgb(31, 29, 46),
                surface_alt: Color::Rgb(38, 35, 58),
                text: Color::Rgb(224, 222, 244),
                muted: Color::Rgb(144, 140, 170),
                accent: Color::Rgb(196, 167, 231),
                success: Color::Rgb(156, 207, 216),
                warn: Color::Rgb(246, 193, 119),
                error: Color::Rgb(235, 111, 146),
                border: Color::Rgb(64, 61, 82),
                border_active: Color::Rgb(82, 79, 103),
            },
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ThemePalette {
    pub base: Color,
    pub surface: Color,
    pub surface_alt: Color,
    pub text: Color,
    pub muted: Color,
    pub accent: Color,
    pub success: Color,
    pub warn: Color,
    pub error: Color,
    pub border: Color,
    pub border_active: Color,
}

pub struct ResolvedTheme {
    pub key: String,
    pub palette: ThemePalette,
}

#[derive(Deserialize)]
struct ThemeFile {
    #[allow(dead_code)]
    name: Option<String>,
    colors: ThemeFileColors,
}

#[derive(Deserialize)]
struct ThemeFileColors {
    base: String,
    surface: String,
    surface_alt: String,
    text: String,
    muted: String,
    accent: String,
    success: String,
    warn: String,
    error: String,
    border: String,
    border_active: String,
}

pub fn resolve_theme(theme_name: &str, config_dir: &Path) -> Result<ResolvedTheme> {
    if let Some(builtin) = ThemeName::from_config(Some(theme_name)) {
        return Ok(ResolvedTheme {
            key: builtin.as_str().to_string(),
            palette: builtin.palette(),
        });
    }

    let file_name = normalize_theme_file_stem(theme_name)?;
    let path = config_dir.join("themes").join(format!("{file_name}.toml"));
    let contents = fs::read_to_string(&path)
        .map_err(|err| anyhow!("Unable to read theme file '{}': {err}", path.display()))?;
    let file: ThemeFile = toml::from_str(&contents)
        .map_err(|err| anyhow!("Invalid theme TOML '{}': {err}", path.display()))?;

    let palette = ThemePalette {
        base: parse_hex_color(&file.colors.base)?,
        surface: parse_hex_color(&file.colors.surface)?,
        surface_alt: parse_hex_color(&file.colors.surface_alt)?,
        text: parse_hex_color(&file.colors.text)?,
        muted: parse_hex_color(&file.colors.muted)?,
        accent: parse_hex_color(&file.colors.accent)?,
        success: parse_hex_color(&file.colors.success)?,
        warn: parse_hex_color(&file.colors.warn)?,
        error: parse_hex_color(&file.colors.error)?,
        border: parse_hex_color(&file.colors.border)?,
        border_active: parse_hex_color(&file.colors.border_active)?,
    };

    Ok(ResolvedTheme {
        key: file_name,
        palette,
    })
}

pub fn discover_file_theme_names(config_dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let themes_dir = config_dir.join("themes");
    let Ok(entries) = fs::read_dir(themes_dir) else {
        return names;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(ext) = path.extension().and_then(|v| v.to_str()) else {
            continue;
        };
        if ext != "toml" {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|v| v.to_str()) else {
            continue;
        };
        if normalize_theme_file_stem(stem).is_ok() {
            names.push(stem.to_string());
        }
    }
    names.sort();
    names.dedup();
    names
}

pub fn config_dir_from_env() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| anyhow!("Unable to determine config directory"))?;
    Ok(base.join("rosie"))
}

fn normalize_theme_file_stem(value: &str) -> Result<String> {
    let stem = value.trim().to_ascii_lowercase();
    if stem.is_empty() {
        return Err(anyhow!("Theme name cannot be empty"));
    }
    if !stem
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
    {
        return Err(anyhow!(
            "Invalid theme name '{}'; use letters, numbers, '-' or '_'",
            value
        ));
    }
    Ok(stem)
}

fn parse_hex_color(value: &str) -> Result<Color> {
    let hex = value.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return Err(anyhow!("Invalid color '{}'; expected #RRGGBB", value));
    }
    let r = u8::from_str_radix(&hex[0..2], 16)
        .map_err(|_| anyhow!("Invalid color '{}'; bad red channel", value))?;
    let g = u8::from_str_radix(&hex[2..4], 16)
        .map_err(|_| anyhow!("Invalid color '{}'; bad green channel", value))?;
    let b = u8::from_str_radix(&hex[4..6], 16)
        .map_err(|_| anyhow!("Invalid color '{}'; bad blue channel", value))?;
    Ok(Color::Rgb(r, g, b))
}
