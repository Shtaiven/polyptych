use std::path::PathBuf;
use std::{env, fs};

use serde::Deserialize;

const DEFAULT_FILE: &str = r##"# polyptych configuration. Changes apply to running windows on save.

# Text shown in the middle of the window.
text = "Spacer, right-click to close"

# Font: a name ("Inter", "Noto Sans Bold"), a generic family ("sans",
# "serif", "monospace") or a file path ("/path/to/font.ttf").
font = "sans"

# Weight from 100 (thin) to 900 (black): 400 is regular, 700 bold. Only
# variable fonts change weight; other fonts are drawn as they are.
font_weight = 400

# Font size in logical pixels.
font_size = 18.0

# Colours are "#RRGGBB" or "#RRGGBBAA".
text_color = "#ffffffff"
background = "#00000059"
"##;

/// Config with colours already converted to premultiplied ARGB8888.
#[derive(Clone, PartialEq)]
pub struct Config {
    pub text: String,
    pub font: String,
    pub font_weight: u16,
    pub font_size: f32,
    pub text_color: u32,
    pub background: u32,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Raw {
    text: String,
    font: String,
    font_weight: u16,
    font_size: f32,
    text_color: String,
    background: String,
}

impl Default for Raw {
    fn default() -> Self {
        Raw {
            text: "Spacer, right-click to close".into(),
            font: "sans".into(),
            font_weight: 400,
            font_size: 18.0,
            text_color: "#ffffffff".into(),
            background: "#00000059".into(),
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
            text: self.text,
            font: self.font,
            font_weight: self.font_weight,
            font_size: self.font_size,
            text_color: parse_color(&self.text_color)?,
            background: parse_color(&self.background)?,
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
    let pm = |c: u8| (c as u32 * a as u32 + 127) / 255;
    Ok((a as u32) << 24 | pm(r) << 16 | pm(g) << 8 | pm(b))
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
}
