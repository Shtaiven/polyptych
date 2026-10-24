//! Loads the idle image: a PNG or JPEG file, or a random one from a folder.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{fs, process};

use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_core::result::DecodingResult;

/// Larger images are refused rather than eating hundreds of MB.
const MAX_SIDE: usize = 8192;

/// Decoded image in premultiplied ARGB8888 (BGRA bytes).
pub struct Image {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Loads `path`, or for a folder a random PNG/JPEG inside it. If the chosen
/// file can't be read, the next one is tried.
pub fn load_any(path: &Path) -> Result<Image, String> {
    if !path.is_dir() {
        return load(path).map_err(|e| format!("{}: {e}", path.display()));
    }
    let mut files: Vec<PathBuf> = fs::read_dir(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                    ["png", "jpg", "jpeg"].contains(&e.to_ascii_lowercase().as_str())
                })
        })
        .collect();
    if files.is_empty() {
        return Err(format!("{}: no PNG or JPEG files", path.display()));
    }
    files.sort();
    // No need for a real RNG: the clock and pid differ for every spacer.
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    let start = (nanos ^ process::id().wrapping_mul(0x9e37_79b9)) as usize % files.len();
    for file in files[start..].iter().chain(&files[..start]) {
        match load(file) {
            Ok(image) => return Ok(image),
            Err(e) => eprintln!("{}: {}: {e}", crate::APP, file.display()),
        }
    }
    Err(format!("{}: no readable images", path.display()))
}

/// Decodes a PNG or JPEG, recognised by its contents rather than its name.
fn load(path: &Path) -> Result<Image, String> {
    let data = fs::read(path).map_err(|e| e.to_string())?;
    let options = DecoderOptions::default()
        .set_max_width(MAX_SIDE)
        .set_max_height(MAX_SIDE)
        .png_set_strip_to_8bit(true)
        .jpeg_set_out_colorspace(ColorSpace::BGRA);

    let (bytes, colorspace, dims) = if data.starts_with(b"\x89PNG") {
        let mut d = zune_png::PngDecoder::new_with_options(ZCursor::new(&data), options);
        let bytes = match d.decode().map_err(|e| format!("{e:?}"))? {
            DecodingResult::U8(bytes) => bytes,
            _ => return Err("unsupported PNG bit depth".into()),
        };
        (bytes, d.colorspace(), d.dimensions())
    } else if data.starts_with(&[0xff, 0xd8, 0xff]) {
        let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(&data), options);
        let bytes = d.decode().map_err(|e| format!("{e:?}"))?;
        (bytes, d.output_colorspace(), d.dimensions())
    } else {
        return Err("not a PNG or JPEG file".into());
    };
    drop(data);
    let (w, h) = dims.ok_or("missing image size")?;

    let pixels = match colorspace {
        // JPEG is already BGRA and opaque, so already premultiplied.
        Some(ColorSpace::BGRA) => bytes,
        Some(ColorSpace::RGBA) => convert(&bytes, 4, |p| [p[0], p[1], p[2], p[3]]),
        Some(ColorSpace::RGB) => convert(&bytes, 3, |p| [p[0], p[1], p[2], 255]),
        Some(ColorSpace::LumaA) => convert(&bytes, 2, |p| [p[0], p[0], p[0], p[1]]),
        Some(ColorSpace::Luma) => convert(&bytes, 1, |p| [p[0], p[0], p[0], 255]),
        other => return Err(format!("unsupported colour space {other:?}")),
    };
    if pixels.len() != w * h * 4 {
        return Err("decoded size doesn't match the image".into());
    }
    Ok(Image { pixels, width: w as u32, height: h as u32 })
}

/// Converts `n`-channel pixels to premultiplied BGRA via an RGBA mapping.
fn convert(bytes: &[u8], n: usize, rgba: impl Fn(&[u8]) -> [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() / n * 4);
    for p in bytes.chunks_exact(n) {
        let [r, g, b, a] = rgba(p);
        let pm = |c: u8| ((c as u32 * a as u32 + 127) / 255) as u8;
        out.extend([pm(b), pm(g), pm(r), a]);
    }
    out
}
