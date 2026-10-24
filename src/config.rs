use std::path::PathBuf;
use std::{env, fs};

use serde::Deserialize;

const DEFAULT_FILE: &str = r##"# polyptych configuration. Changes apply to running windows on save.

# The spacer always shows its idle look. When it is focused, hovered or
# floating, focus_color and focus_text are drawn over it.

# Idle look: a colour, with an optional image drawn over it. The colour
# shows through transparent parts of the image and around a zoomed one.
idle_color = "#00000000"
# A PNG or JPEG file, or a folder to pick a random one from each time a
# spacer opens. Relative paths are relative to this file's folder.
# idle_image = "spacers"
# How the image fills the window: "zoom" (fit inside), "fill" (cover and
# crop), "stretch" or "tile".
idle_image_fit = "fill"

# Text shown in the middle of the window when focused.
focus_text = "Spacer, right-click to close"

# Font: a name ("Inter", "Noto Sans Bold"), a generic family ("sans",
# "serif", "monospace") or a file path ("/path/to/font.ttf").
font = "sans"

# Weight from 100 (thin) to 900 (black): 400 is regular, 700 bold. Only
# variable fonts change weight; other fonts are drawn as they are.
font_weight = 400

# Font size in logical pixels.
font_size = 18.0

# Colours are "#RRGGBB" or "#RRGGBBAA". A transparent focus_color lets the
# idle look show through.
text_color = "#ffffffff"
focus_color = "#00000059"
"##;

/// Config with colours already converted to premultiplied ARGB8888.
#[derive(Clone, PartialEq)]
pub struct Config {
    pub idle_color: u32,
    pub idle_image: Option<PathBuf>,
    pub idle_image_fit: Fit,
    pub focus_text: String,
    pub font: String,
    pub font_weight: u16,
    pub font_size: f32,
    pub text_color: u32,
    pub focus_color: u32,
}

/// How the idle image is fitted to the window.
#[derive(Clone, Copy, PartialEq, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Fit {
    /// Scaled to fit inside, keeping its aspect ratio.
    Zoom,
    /// Scaled to cover the window, cropping the overflow.
    Fill,
    Stretch,
    /// Repeated at its own size from the top left.
    Tile,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Raw {
    idle_color: String,
    idle_image: Option<String>,
    idle_image_fit: Fit,
    focus_text: String,
    font: String,
    font_weight: u16,
    font_size: f32,
    text_color: String,
    focus_color: String,
}

impl Default for Raw {
    fn default() -> Self {
        Raw {
            idle_color: "#00000000".into(),
            idle_image: None,
            idle_image_fit: Fit::Fill,
            focus_text: "Spacer, right-click to close".into(),
            font: "sans".into(),
            font_weight: 400,
            font_size: 18.0,
            text_color: "#ffffffff".into(),
            focus_color: "#00000059".into(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Raw::default().resolve().expect("default config is valid")
    }
}

impl Raw {
    fn resolve(self) -> Result<Config, String> {
        if !(self.font_size > 0.0 && self.font_size <= 512.0) {
            return Err(format!("font_size must be in (0, 512], got {}", self.font_size));
        }
        if !(1..=1000).contains(&self.font_weight) {
            return Err(format!("font_weight must be in 1..=1000, got {}", self.font_weight));
        }
        Ok(Config {
            idle_color: parse_color(&self.idle_color)?,
            idle_image: self.idle_image.as_deref().and_then(resolve_path),
            idle_image_fit: self.idle_image_fit,
            focus_text: self.focus_text,
            font: self.font,
            font_weight: self.font_weight,
            font_size: self.font_size,
            text_color: parse_color(&self.text_color)?,
            focus_color: parse_color(&self.focus_color)?,
        })
    }
}

/// Parses "#RRGGBB" or "#RRGGBBAA" into premultiplied 0xAARRGGBB.
fn parse_color(s: &str) -> Result<u32, String> {
    let hex = s.strip_prefix('#').unwrap_or(s);
    let bad = || format!("invalid colour {s:?}, expected #RRGGBB or #RRGGBBAA");
    let v = u32::from_str_radix(hex, 16).map_err(|_| bad())?;
    let rgba = match hex.len() {
        6 => (v << 8) | 0xff,
        8 => v,
        _ => return Err(bad()),
    };
    let [r, g, b, a] = rgba.to_be_bytes();
    let pm = |c| premultiply(c, a) as u32;
    Ok((a as u32) << 24 | pm(r) << 16 | pm(g) << 8 | pm(b))
}

/// Scales a colour channel by alpha, rounding to nearest.
pub fn premultiply(c: u8, a: u8) -> u8 {
    ((c as u32 * a as u32 + 127) / 255) as u8
}

/// Expands `~/`, and resolves relative paths against the config folder. An
/// empty path means none.
fn resolve_path(s: &str) -> Option<PathBuf> {
    let s = s.trim();
    (!s.is_empty()).then(|| crate::font::as_path(s).unwrap_or_else(|| dir().join(s)))
}

pub fn dir() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join(crate::APP)
}

pub fn file_name() -> String {
    format!("{}.toml", crate::APP)
}

/// Writes the commented default config if none exists yet.
pub fn ensure_exists() {
    let dir = dir();
    let path = dir.join(file_name());
    if !path.exists() {
        let _ = fs::create_dir_all(&dir).and_then(|_| fs::write(&path, DEFAULT_FILE));
    }
}

/// Loads the config, returning an error message on bad TOML or values.
pub fn load() -> Result<Config, String> {
    let path = dir().join(file_name());
    let src = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    toml::from_str::<Raw>(&src)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .resolve()
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours() {
        assert_eq!(parse_color("#ffffff").unwrap(), 0xffffffff);
        assert_eq!(parse_color("#00000059").unwrap(), 0x59000000);
        assert_eq!(parse_color("#ff000080").unwrap(), 0x80800000);
        assert!(parse_color("#fff").is_err());
        assert!(parse_color("#gggggg").is_err());
    }

    #[test]
    fn default_file_parses() {
        let c = toml::from_str::<Raw>(DEFAULT_FILE).unwrap().resolve().unwrap();
        assert!(c == Config::default());
    }

    #[test]
    fn idle_options() {
        let parse = |s: &str| toml::from_str::<Raw>(s).map_err(|e| e.to_string())?.resolve();
        assert!(parse(r#"idle_image_fit = "tile""#).unwrap().idle_image_fit == Fit::Tile);
        assert!(parse(r#"idle_image_fit = "bogus""#).is_err());
        assert!(parse(r#"text = "old name""#).is_err());
        let abs = parse(r#"idle_image = "/a/b""#).unwrap().idle_image;
        assert_eq!(abs, Some(PathBuf::from("/a/b")));
        let rel = parse(r#"idle_image = "spacers""#).unwrap().idle_image;
        assert_eq!(rel, Some(dir().join("spacers")));
    }
}
