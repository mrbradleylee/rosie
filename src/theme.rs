use ratatui::style::Color;

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
                name: self,
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
                name: self,
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
    pub name: ThemeName,
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
