//! Finds a system font without bundling one or linking fontconfig.
//!
//! A query is tried as a file path, then with `fc-match`, then by scanning the
//! XDG font directories and matching font names read from each file.

use std::env;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::process::Command;

use ttf_parser::name::Table;
use ttf_parser::name_id;

/// A font face on disk: the file and the face index within it.
#[derive(Clone, Debug, PartialEq)]
pub struct Face {
    pub path: PathBuf,
    pub index: u32,
}

const SANS: &[&str] = &[
    "Noto Sans",
    "DejaVu Sans",
    "Adwaita Sans",
    "Liberation Sans",
    "Cantarell",
    "Inter",
    "Open Sans",
    "Roboto",
    "Ubuntu",
    "Fira Sans",
    "FreeSans",
];
const SERIF: &[&str] = &[
    "Noto Serif",
    "DejaVu Serif",
    "Liberation Serif",
    "Source Serif 4",
    "FreeSerif",
];
const MONO: &[&str] = &[
    "Noto Sans Mono",
    "DejaVu Sans Mono",
    "Adwaita Mono",
    "Liberation Mono",
    "Fira Mono",
    "Source Code Pro",
    "FreeMono",
];

/// Finds a face for a font name ("Inter"), a generic family ("sans", "serif",
/// "monospace") or a file path.
pub fn find(query: &str) -> Option<Face> {
    let query = query.trim();
    if let Some(path) = as_path(query) {
        return path.is_file().then_some(Face { path, index: 0 });
    }
    if let Some(face) = fc_match(query) {
        return Some(face);
    }

    let files = font_files();
    match generic(query) {
        Some(families) => families
            .iter()
            .find_map(|family| by_name(&files, family))
            .or_else(|| {
                files.first().map(|p| Face {
                    path: p.clone(),
                    index: 0,
                })
            }),
        None => by_name(&files, query),
    }
}

/// An absolute or `~/` path; `None` for anything else, such as a font name.
pub(crate) fn as_path(query: &str) -> Option<PathBuf> {
    if query.starts_with('/') {
        Some(PathBuf::from(query))
    } else {
        let rest = query.strip_prefix("~/")?;
        Some(PathBuf::from(env::var_os("HOME")?).join(rest))
    }
}

fn generic(query: &str) -> Option<&'static [&'static str]> {
    match query.to_ascii_lowercase().as_str() {
        "sans" | "sans-serif" | "sans serif" => Some(SANS),
        "serif" => Some(SERIF),
        "mono" | "monospace" => Some(MONO),
        _ => None,
    }
}

/// Asks fontconfig's CLI. Fails quietly when `fc-match` isn't installed.
fn fc_match(family: &str) -> Option<Face> {
    let out = Command::new("fc-match")
        .args(["-f", "%{file}\n%{index}\n%{family}", family])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let out = String::from_utf8(out.stdout).ok()?;
    let mut lines = out.split('\n');
    let (file, index) = (lines.next()?, lines.next().unwrap_or("0"));
    let families = lines.next().unwrap_or_default();
    // fc-match always answers, substituting a default for unknown names.
    let want = normalize(family);
    if generic(family).is_none() && !families.split(',').any(|f| want == normalize(f)) {
        eprintln!(
            "{}: font {family:?} not installed, fontconfig chose {families:?}",
            crate::APP
        );
    }
    let path = PathBuf::from(file);
    // The upper 16 bits name a variable font's named instance; the weight is
    // applied directly instead.
    let index = index.trim().parse::<u32>().unwrap_or(0) & 0xffff;
    path.is_file().then_some(Face { path, index })
}

/// Lists font files in the XDG font directories, sorted for stable results.
fn font_files() -> Vec<PathBuf> {
    let home = env::var_os("HOME").map(PathBuf::from);
    let var = |name| env::var_os(name).filter(|v| !v.is_empty());
    let mut dirs = Vec::new();
    match (var("XDG_DATA_HOME"), &home) {
        (Some(d), _) => dirs.push(PathBuf::from(d).join("fonts")),
        (None, Some(h)) => dirs.push(h.join(".local/share/fonts")),
        _ => {}
    }
    if let Some(h) = &home {
        dirs.push(h.join(".fonts"));
    }
    let data_dirs = var("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(env::split_paths(&data_dirs).map(|d| d.join("fonts")));

    let mut files = Vec::new();
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                dirs.push(path);
            } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                ["ttf", "otf", "ttc", "otc"].contains(&e.to_ascii_lowercase().as_str())
            }) {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// Finds the best face whose names match `name`. Files whose file name looks
/// right are checked first, so usually only a handful of files are opened.
fn by_name(files: &[PathBuf], name: &str) -> Option<Face> {
    let want = normalize(name);
    let (likely, rest): (Vec<_>, Vec<_>) = files.iter().partition(|p| {
        p.file_stem()
            .is_some_and(|s| normalize(&s.to_string_lossy()).contains(&want))
    });
    best_match(&likely, &want).or_else(|| best_match(&rest, &want))
}

fn best_match(files: &[&PathBuf], want: &str) -> Option<Face> {
    let mut best: Option<(u8, Face)> = None;
    for path in files {
        for (index, names) in faces(path) {
            let score = names.score(want);
            if score > best.as_ref().map_or(0, |b| b.0) {
                best = Some((
                    score,
                    Face {
                        path: (*path).clone(),
                        index,
                    },
                ));
                if score == 3 {
                    return best.map(|b| b.1);
                }
            }
        }
    }
    best.map(|b| b.1)
}

/// Lowercase alphanumerics only, so "Noto Sans" matches "NotoSans-Regular".
fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[derive(Default)]
struct Names {
    family: String,
    style: String,
    full: String,
}

impl Names {
    /// 3: exact face ("Noto Sans Bold") or the regular face of the family.
    /// 2: another style of the family. 0: no match.
    fn score(&self, want: &str) -> u8 {
        let regular =
            ["", "regular", "book", "normal", "roman"].contains(&normalize(&self.style).as_str());
        if normalize(&self.full) == want || (normalize(&self.family) == want && regular) {
            3
        } else if normalize(&self.family) == want {
            2
        } else {
            0
        }
    }
}

/// Reads the names of every face in a font file, touching only the header and
/// `name` table rather than the whole file.
fn faces(path: &PathBuf) -> Vec<(u32, Names)> {
    let Ok(mut file) = File::open(path) else {
        return Vec::new();
    };
    let Some(head) = read_at(&mut file, 0, 12) else {
        return Vec::new();
    };
    let offsets = if &head[..4] == b"ttcf" {
        let count = be32(&head[8..]).min(256) as usize;
        read_at(&mut file, 12, count * 4)
            .map(|raw| raw.chunks_exact(4).map(be32).collect())
            .unwrap_or_default()
    } else {
        vec![0]
    };
    (0..)
        .zip(offsets)
        .filter_map(|(i, off)| Some((i, face_names(&mut file, off as u64)?)))
        .collect()
}

fn face_names(file: &mut File, offset: u64) -> Option<Names> {
    let header = read_at(file, offset, 12)?;
    let tables = u16::from_be_bytes([header[4], header[5]]) as usize;
    let records = read_at(file, offset + 12, tables * 16)?;
    let record = records.chunks_exact(16).find(|r| &r[..4] == b"name")?;
    let data = read_at(
        file,
        be32(&record[8..]) as u64,
        be32(&record[12..]) as usize,
    )?;
    let table = Table::parse(&data)?;

    // Prefer US English names, else take the first readable one of each kind.
    let mut slots: [Option<(bool, String)>; 18] = Default::default();
    for name in table.names {
        let id = name.name_id as usize;
        let Some(text) = (id < slots.len()).then(|| name.to_string()).flatten() else {
            continue;
        };
        let english = name.language_id == 0x409;
        if slots[id].as_ref().is_none_or(|(en, _)| english && !en) {
            slots[id] = Some((english, text));
        }
    }
    let mut take = |id: u16| slots[id as usize].take().map(|s| s.1);
    let full = take(name_id::FULL_NAME).unwrap_or_default();
    // Typographic names group weights under one family ("Noto Sans" + "Light")
    // where the legacy names would say "Noto Sans Light" + "Regular".
    let (family, style) = match take(name_id::TYPOGRAPHIC_FAMILY) {
        Some(family) => (
            family,
            take(name_id::TYPOGRAPHIC_SUBFAMILY).unwrap_or_default(),
        ),
        None => (
            take(name_id::FAMILY)?,
            take(name_id::SUBFAMILY).unwrap_or_default(),
        ),
    };
    Some(Names {
        family,
        style,
        full,
    })
}

fn read_at(file: &mut File, offset: u64, len: usize) -> Option<Vec<u8>> {
    let mut buf = vec![0; len];
    file.seek(SeekFrom::Start(offset)).ok()?;
    file.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoring() {
        let names = |family: &str, style: &str, full: &str| Names {
            family: family.into(),
            style: style.into(),
            full: full.into(),
        };
        let want = normalize("Noto Sans");
        assert_eq!(
            names("Noto Sans", "Regular", "Noto Sans Regular").score(&want),
            3
        );
        assert_eq!(
            names("Noto Sans", "Light", "Noto Sans Light").score(&want),
            2
        );
        assert_eq!(names("Noto Serif", "Regular", "Noto Serif").score(&want), 0);
        let bold = normalize("Noto Sans Bold");
        assert_eq!(names("Noto Sans", "Bold", "Noto Sans Bold").score(&bold), 3);
    }

    #[test]
    fn paths() {
        assert_eq!(as_path("/a/b.ttf"), Some(PathBuf::from("/a/b.ttf")));
        assert!(as_path("Noto Sans").is_none());
    }
}
