use anyhow::{Result, anyhow};
use ratatui::style::Color;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_THEME_KEY: &str = "rose-pine";

pub fn default_theme() -> ResolvedTheme {
    let packaged_dir = packaged_theme_dir();
    match resolve_theme_from_dir(DEFAULT_THEME_KEY, &packaged_dir) {
        Ok(Some(theme)) => theme,
        _ => ResolvedTheme {
            key: DEFAULT_THEME_KEY.to_string(),
            palette: ThemePalette {
                base: Color::Rgb(25, 23, 36),
                surface: Color::Rgb(31, 29, 46),
                surface_alt: Color::Rgb(38, 35, 58),
                text: Color::Rgb(224, 222, 244),
                muted: Color::Rgb(144, 140, 170),
                accent: Color::Rgb(196, 167, 231),
                success: Color::Rgb(156, 207, 216),
                info: Color::Rgb(49, 116, 143),
                warn: Color::Rgb(246, 193, 119),
                error: Color::Rgb(235, 111, 146),
                border: Color::Rgb(64, 61, 82),
                border_active: Color::Rgb(82, 79, 103),
                user: Color::Rgb(196, 167, 231),
                assistant: Color::Rgb(224, 222, 244),
                system: Color::Rgb(144, 140, 170),
                highlight_low: Color::Rgb(33, 32, 46),
                highlight_mid: Color::Rgb(64, 61, 82),
                highlight_high: Color::Rgb(82, 79, 103),
                title_label: Color::Rgb(224, 222, 244),
                title_value: Color::Rgb(196, 167, 231),
                title_value_alt: Color::Rgb(235, 188, 186),
                title_meta: Color::Rgb(144, 140, 170),
            },
        },
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
    pub info: Color,
    pub warn: Color,
    pub error: Color,
    pub border: Color,
    pub border_active: Color,
    pub user: Color,
    pub assistant: Color,
    pub system: Color,
    pub highlight_low: Color,
    pub highlight_mid: Color,
    pub highlight_high: Color,
    pub title_label: Color,
    pub title_value: Color,
    pub title_value_alt: Color,
    pub title_meta: Color,
}

pub struct ResolvedTheme {
    pub key: String,
    pub palette: ThemePalette,
}

#[derive(Deserialize)]
struct ThemeFile {
    #[allow(dead_code)]
    name: Option<String>,
    ui: Option<ThemeFileUi>,
    state: Option<ThemeFileState>,
    #[allow(dead_code)]
    syntax: Option<ThemeFileSyntax>,
    #[allow(dead_code)]
    highlight: Option<ThemeFileHighlight>,
    colors: Option<ThemeFileColors>,
}

#[derive(Deserialize)]
struct ThemeFileUi {
    bg: String,
    panel: String,
    panel_alt: String,
    text: String,
    text_muted: String,
    border: String,
    border_active: String,
    title_label: Option<String>,
    title_value: Option<String>,
    title_value_alt: Option<String>,
    title_meta: Option<String>,
}

#[derive(Deserialize)]
struct ThemeFileState {
    accent: String,
    success: String,
    info: Option<String>,
    warning: String,
    error: String,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct ThemeFileSyntax {
    user: String,
    assistant: String,
    system: String,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct ThemeFileHighlight {
    low: String,
    mid: String,
    high: String,
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
    let file_name = normalize_theme_file_stem(theme_name)?;
    if let Some(theme) = resolve_theme_from_dir(&file_name, &config_dir.join("themes"))? {
        return Ok(theme);
    }
    if let Some(theme) = resolve_theme_from_dir(&file_name, &packaged_theme_dir())? {
        return Ok(theme);
    }
    Err(anyhow!(
        "Theme '{file_name}' not found in '{}' or '{}'",
        config_dir.join("themes").display(),
        packaged_theme_dir().display()
    ))
}

fn parse_theme_palette(file: &ThemeFile, path: &Path) -> Result<ThemePalette> {
    if let (Some(ui), Some(state)) = (&file.ui, &file.state) {
        let base = parse_hex_color(&ui.bg)?;
        let surface = parse_hex_color(&ui.panel)?;
        let surface_alt = parse_hex_color(&ui.panel_alt)?;
        let text = parse_hex_color(&ui.text)?;
        let muted = parse_hex_color(&ui.text_muted)?;
        let accent = parse_hex_color(&state.accent)?;
        let success = parse_hex_color(&state.success)?;
        let info = state
            .info
            .as_deref()
            .map(parse_hex_color)
            .transpose()?
            .unwrap_or(accent);
        let warn = parse_hex_color(&state.warning)?;
        let error = parse_hex_color(&state.error)?;
        let border = parse_hex_color(&ui.border)?;
        let border_active = parse_hex_color(&ui.border_active)?;
        let user = file
            .syntax
            .as_ref()
            .map(|syntax| parse_hex_color(&syntax.user))
            .transpose()?
            .unwrap_or(accent);
        let assistant = file
            .syntax
            .as_ref()
            .map(|syntax| parse_hex_color(&syntax.assistant))
            .transpose()?
            .unwrap_or(text);
        let system = file
            .syntax
            .as_ref()
            .map(|syntax| parse_hex_color(&syntax.system))
            .transpose()?
            .unwrap_or(muted);
        let highlight_low = file
            .highlight
            .as_ref()
            .map(|highlight| parse_hex_color(&highlight.low))
            .transpose()?
            .unwrap_or(base);
        let highlight_mid = file
            .highlight
            .as_ref()
            .map(|highlight| parse_hex_color(&highlight.mid))
            .transpose()?
            .unwrap_or(border);
        let highlight_high = file
            .highlight
            .as_ref()
            .map(|highlight| parse_hex_color(&highlight.high))
            .transpose()?
            .unwrap_or(border_active);
        let title_label = ui
            .title_label
            .as_deref()
            .map(parse_hex_color)
            .transpose()?
            .unwrap_or(text);
        let title_value = ui
            .title_value
            .as_deref()
            .map(parse_hex_color)
            .transpose()?
            .unwrap_or(accent);
        let title_value_alt = ui
            .title_value_alt
            .as_deref()
            .map(parse_hex_color)
            .transpose()?
            .unwrap_or(title_value);
        let title_meta = ui
            .title_meta
            .as_deref()
            .map(parse_hex_color)
            .transpose()?
            .unwrap_or(muted);
        return Ok(ThemePalette {
            base,
            surface,
            surface_alt,
            text,
            muted,
            accent,
            success,
            info,
            warn,
            error,
            border,
            border_active,
            user,
            assistant,
            system,
            highlight_low,
            highlight_mid,
            highlight_high,
            title_label,
            title_value,
            title_value_alt,
            title_meta,
        });
    }

    if let Some(colors) = &file.colors {
        let base = parse_hex_color(&colors.base)?;
        let surface = parse_hex_color(&colors.surface)?;
        let surface_alt = parse_hex_color(&colors.surface_alt)?;
        let text = parse_hex_color(&colors.text)?;
        let muted = parse_hex_color(&colors.muted)?;
        let accent = parse_hex_color(&colors.accent)?;
        let success = parse_hex_color(&colors.success)?;
        let info = accent;
        let warn = parse_hex_color(&colors.warn)?;
        let error = parse_hex_color(&colors.error)?;
        let border = parse_hex_color(&colors.border)?;
        let border_active = parse_hex_color(&colors.border_active)?;
        return Ok(ThemePalette {
            base,
            surface,
            surface_alt,
            text,
            muted,
            accent,
            success,
            info,
            warn,
            error,
            border,
            border_active,
            user: accent,
            assistant: text,
            system: muted,
            highlight_low: base,
            highlight_mid: border,
            highlight_high: border_active,
            title_label: text,
            title_value: accent,
            title_value_alt: accent,
            title_meta: muted,
        });
    }

    Err(anyhow!(
        "Theme file '{}' must define either [ui] + [state] sections or legacy [colors] section",
        path.display()
    ))
}

pub fn discover_config_theme_names(config_dir: &Path) -> Vec<String> {
    let mut names = discover_file_theme_names_in_dir(&config_dir.join("themes"));
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

fn packaged_theme_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("themes")
}

fn discover_file_theme_names_in_dir(dir: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
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
    names
}

fn resolve_theme_from_dir(theme_name: &str, dir: &Path) -> Result<Option<ResolvedTheme>> {
    let path = dir.join(format!("{theme_name}.toml"));
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(anyhow!(
                "Unable to read theme file '{}': {err}",
                path.display()
            ));
        }
    };
    let file: ThemeFile = toml::from_str(&contents)
        .map_err(|err| anyhow!("Invalid theme TOML '{}': {err}", path.display()))?;
    let palette = parse_theme_palette(&file, &path)?;
    Ok(Some(ResolvedTheme {
        key: theme_name.to_string(),
        palette,
    }))
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
