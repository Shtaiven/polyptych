//! Loads the idle image: a PNG or JPEG file, or a random one from a folder.
//!
//! Images are decoded straight into the shm pool they are shown from, so only
//! one full-size copy ever exists.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{fs, process};

use smithay_client_toolkit::shm::{Shm, raw::RawPool};
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;

use crate::config::premultiply;

/// Reads one decoded pixel as RGBA.
type ToRgba = fn(&[u8]) -> [u8; 4];

/// Larger images are refused rather than eating hundreds of MB.
const MAX_SIDE: usize = 8192;

/// A decoded image in premultiplied ARGB8888, filling the start of `pool`.
pub struct Image {
    pub pool: RawPool,
    pub width: u32,
    pub height: u32,
}

/// Loads `path`, or for a folder a random PNG/JPEG inside it. If the chosen
/// file can't be read, the next one is tried.
pub fn load_any(path: &Path, shm: &Shm) -> Result<Image, String> {
    if !path.is_dir() {
        return load(path, shm).map_err(|e| format!("{}: {e}", path.display()));
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
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let start = (nanos ^ process::id().wrapping_mul(0x9e37_79b9)) as usize % files.len();
    for file in files[start..].iter().chain(&files[..start]) {
        match load(file, shm) {
            Ok(image) => return Ok(image),
            Err(e) => eprintln!("{}: {}: {e}", crate::APP, file.display()),
        }
    }
    Err(format!("{}: no readable images", path.display()))
}

/// Decodes a PNG or JPEG, recognised by its contents rather than its name.
fn load(path: &Path, shm: &Shm) -> Result<Image, String> {
    let data = fs::read(path).map_err(|e| e.to_string())?;
    let options = DecoderOptions::default()
        .set_max_width(MAX_SIDE)
        .set_max_height(MAX_SIDE)
        .png_set_strip_to_8bit(true)
        .jpeg_set_out_colorspace(ColorSpace::BGRA);

    if data.starts_with(b"\x89PNG") {
        let mut d = zune_png::PngDecoder::new_with_options(ZCursor::new(&data), options);
        d.decode_headers().map_err(|e| format!("{e:?}"))?;
        let (dims, colorspace) = (d.dimensions(), d.colorspace());
        decode_into_pool(dims, colorspace, shm, |out| {
            d.decode_into(out).map_err(|e| format!("{e:?}"))
        })
    } else if data.starts_with(&[0xff, 0xd8, 0xff]) {
        let mut d = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(&data), options);
        d.decode_headers().map_err(|e| format!("{e:?}"))?;
        let (dims, colorspace) = (d.dimensions(), d.output_colorspace());
        decode_into_pool(dims, colorspace, shm, |out| {
            d.decode_into(out).map_err(|e| format!("{e:?}"))
        })
    } else {
        Err("not a PNG or JPEG file".into())
    }
}

/// Decodes into the tail of a pool sized for BGRA, then expands in place.
fn decode_into_pool(
    dims: Option<(usize, usize)>,
    colorspace: Option<ColorSpace>,
    shm: &Shm,
    decode: impl FnOnce(&mut [u8]) -> Result<(), String>,
) -> Result<Image, String> {
    let (w, h) = dims.ok_or("missing image size")?;
    let colorspace = colorspace.ok_or("missing colour space")?;
    // How to read each decoded pixel as RGBA; `None` if it needs no conversion.
    let rgba: Option<ToRgba> = match colorspace {
        // JPEG decodes to BGRA directly and is opaque, so already premultiplied.
        ColorSpace::BGRA => None,
        ColorSpace::RGBA => Some(|p| [p[0], p[1], p[2], p[3]]),
        ColorSpace::RGB => Some(|p| [p[0], p[1], p[2], 255]),
        ColorSpace::LumaA => Some(|p| [p[0], p[0], p[0], p[1]]),
        ColorSpace::Luma => Some(|p| [p[0], p[0], p[0], 255]),
        other => return Err(format!("unsupported colour space {other:?}")),
    };
    let n = colorspace.num_components();
    let len = w * h * 4;
    // One spare row: libwayland rejects a buffer whose offset plus stride ×
    // height passes the pool's end, which a cropped view of the last rows
    // does by up to one row.
    let mut pool = RawPool::new(len + w * 4, shm).map_err(|e| format!("no memory: {e}"))?;
    let pixels = &mut pool.mmap()[..len];
    decode(&mut pixels[len - w * h * n..])?;
    if let Some(rgba) = rgba {
        expand(pixels, n, rgba);
    }
    Ok(Image {
        pool,
        width: w as u32,
        height: h as u32,
    })
}

/// Expands `n`-channel pixels packed at the end of `pixels` to premultiplied
/// BGRA filling all of it. Each write lands at or before the pixel it reads,
/// so this works in place.
fn expand(pixels: &mut [u8], n: usize, rgba: ToRgba) {
    let count = pixels.len() / 4;
    let tail = pixels.len() - count * n;
    for i in 0..count {
        let [r, g, b, a] = rgba(&pixels[tail + i * n..][..n]);
        let pm = |c| premultiply(c, a);
        pixels[i * 4..][..4].copy_from_slice(&[pm(b), pm(g), pm(r), a]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_in_place() {
        // Two RGBA pixels: opaque red and half-transparent white.
        let mut px = [255, 0, 0, 255, 255, 255, 255, 128];
        expand(&mut px, 4, |p| [p[0], p[1], p[2], p[3]]);
        assert_eq!(px, [0, 0, 255, 255, 128, 128, 128, 128]);

        // Two grey pixels packed at the end.
        let mut px = [0, 0, 0, 0, 0, 0, 10, 200];
        expand(&mut px, 1, |p| [p[0], p[0], p[0], 255]);
        assert_eq!(px, [10, 10, 10, 255, 200, 200, 200, 255]);

        // Two RGB pixels packed at the end.
        let mut px = [0, 0, 1, 2, 3, 4, 5, 6];
        expand(&mut px, 3, |p| [p[0], p[1], p[2], 255]);
        assert_eq!(px, [3, 2, 1, 255, 6, 5, 4, 255]);
    }
}
