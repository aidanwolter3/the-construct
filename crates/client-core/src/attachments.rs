//! Staged attachments (docs/DECISIONS.md D49, docs/posting.md): a picked photo
//! is decoded, turned upright (its EXIF orientation applied), scaled so its
//! longer side is at most [`MAX_SIDE`] px and written as a JPEG with no
//! metadata at all (no GPS, no camera details) under the engine's
//! attachments directory. Only that copy is ever uploaded.

use std::fs;
use std::io::{BufWriter, Cursor};
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{DynamicImage, ImageDecoder, ImageReader};

/// Longest side of a staged photo, in pixels.
pub const MAX_SIDE: u32 = 2048;
/// JPEG quality of a staged photo.
pub const JPEG_QUALITY: u8 = 88;
/// Largest source file accepted (a picked photo, before re-encoding).
pub const MAX_SOURCE_BYTES: u64 = 60 * 1024 * 1024;
/// Largest decoded image accepted (width × height), against decompression
/// bombs.
pub const MAX_PIXELS: u64 = 120_000_000;
/// URI scheme of the local entity drawn for an attachment.
pub const ENTITY_PREFIX: &str = "construct://attachments/";
/// MIME type of every staged file.
pub const STAGED_MIME: &str = "image/jpeg";

/// The entity URI of attachment `id`.
pub fn entity_uri(id: &str) -> String {
    format!("{ENTITY_PREFIX}{id}")
}

/// A `file://` URL for a staged file (what image loaders draw).
pub fn file_url(path: &Path) -> String {
    reqwest::Url::from_file_path(path)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| format!("file://{}", path.display()))
}

/// A freshly staged file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    pub path: PathBuf,
    pub size: u64,
    pub width: u32,
    pub height: u32,
}

/// Why a file could not be staged (shown to the user).
pub fn stage(src: &Path, dir: &Path, id: &str) -> Result<Staged, String> {
    let meta = fs::metadata(src).map_err(|e| format!("can't read the photo ({e})"))?;
    if !meta.is_file() {
        return Err("that isn't a file".into());
    }
    if meta.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "the photo is too large ({} MB, at most {} MB)",
            meta.len() / (1024 * 1024),
            MAX_SOURCE_BYTES / (1024 * 1024)
        ));
    }
    let bytes = fs::read(src).map_err(|e| format!("can't read the photo ({e})"))?;
    let img = decode_upright(&bytes)?;
    let img = if img.width().max(img.height()) > MAX_SIDE {
        img.resize(MAX_SIDE, MAX_SIDE, FilterType::Triangle)
    } else {
        img
    };
    // JPEG has no alpha: flatten onto white rather than black.
    let rgb = flatten(img);
    crate::store::make_private_dir(dir).map_err(|e| format!("can't save the photo ({e})"))?;
    let path = dir.join(format!("{id}.jpg"));
    let tmp = dir.join(format!("{id}.jpg.part"));
    let written = (|| {
        let file = fs::File::create(&tmp)?;
        let mut out = BufWriter::new(file);
        JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY)
            .encode_image(&rgb)
            .map_err(std::io::Error::other)?;
        out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        fs::rename(&tmp, &path)
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(format!("can't save the photo ({e})"));
    }
    let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    Ok(Staged {
        path,
        size,
        width: rgb.width(),
        height: rgb.height(),
    })
}

/// Decode `bytes` (JPEG, PNG, WebP or GIF), with the EXIF orientation applied.
fn decode_upright(bytes: &[u8]) -> Result<DynamicImage, String> {
    let unsupported = "this kind of image isn't supported (use JPEG, PNG or WebP)";
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| unsupported.to_string())?;
    if reader.format().is_none() {
        return Err(unsupported.into());
    }
    let mut decoder = reader.into_decoder().map_err(|_| unsupported.to_string())?;
    let (w, h) = decoder.dimensions();
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err("the photo's size isn't supported".into());
    }
    let orientation = decoder.orientation().ok();
    let mut img = DynamicImage::from_decoder(decoder)
        .map_err(|e| format!("the photo couldn't be read ({e})"))?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    Ok(img)
}

fn flatten(img: DynamicImage) -> image::RgbImage {
    if !img.color().has_alpha() {
        return img.into_rgb8();
    }
    let rgba = img.into_rgba8();
    let mut out = image::RgbImage::new(rgba.width(), rgba.height());
    for (o, p) in out.pixels_mut().zip(rgba.pixels()) {
        let a = u16::from(p[3]);
        for c in 0..3 {
            o[c] = ((u16::from(p[c]) * a + 255 * (255 - a)) / 255) as u8;
        }
    }
    out
}

/// Replace every JSON string exactly `"$attachments.<name>"` with `with(name)`
/// (a list); other strings are kept.
pub fn substitute(
    value: serde_json::Value,
    with: &dyn Fn(&str) -> Option<serde_json::Value>,
) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::String(s) => match s.strip_prefix("$attachments.").and_then(with) {
            Some(v) => v,
            None => Value::String(s),
        },
        Value::Array(items) => {
            Value::Array(items.into_iter().map(|v| substitute(v, with)).collect())
        }
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, substitute(v, with)))
                .collect(),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn jpeg_with_orientation(w: u32, h: u32, orientation: u16) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, _| image::Rgb([(x % 256) as u8, 10, 200]));
        let mut plain = Vec::new();
        JpegEncoder::new_with_quality(&mut plain, 90)
            .encode_image(&img)
            .unwrap();
        // Insert an APP1 Exif segment (big-endian TIFF, one IFD entry:
        // Orientation) and a GPS-looking marker string right after SOI.
        let mut tiff = vec![b'M', b'M', 0, 42, 0, 0, 0, 8, 0, 1];
        tiff.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1]);
        tiff.extend_from_slice(&orientation.to_be_bytes());
        tiff.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        tiff.extend_from_slice(b"GPSSECRET");
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&tiff);
        let len = (app1.len() + 2) as u16;
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&app1);
        out.extend_from_slice(&plain[2..]);
        out
    }

    #[test]
    fn stage_rotates_scales_and_strips_metadata() {
        let dir = std::env::temp_dir().join(format!("construct-att-test-{}", std::process::id()));
        let src = dir.join("src.jpg");
        fs::create_dir_all(&dir).unwrap();
        // 3000x1000, orientation 6 (rotate 90° clockwise to view).
        fs::write(&src, jpeg_with_orientation(3000, 1000, 6)).unwrap();
        let s = stage(&src, &dir.join("out"), "abc").unwrap();
        assert_eq!((s.width, s.height), (683, 2048));
        let out = fs::read(&s.path).unwrap();
        assert!(!out.windows(9).any(|w| w == b"GPSSECRET"));
        assert!(!out.windows(4).any(|w| w == b"Exif"));
        assert_eq!(image::load_from_memory(&out).unwrap().width(), 683);
        // Small PNGs with alpha keep their size and become JPEGs.
        let png = dir.join("a.png");
        image::RgbaImage::from_pixel(10, 20, image::Rgba([0, 0, 0, 0]))
            .save(&png)
            .unwrap();
        let s = stage(&png, &dir.join("out"), "png").unwrap();
        assert_eq!((s.width, s.height), (10, 20));
        let back = image::load_from_memory(&fs::read(&s.path).unwrap())
            .unwrap()
            .into_rgb8();
        assert!(back.get_pixel(5, 5)[0] > 240, "transparent becomes white");
        // Not an image.
        let txt = dir.join("x.txt");
        fs::write(&txt, b"hello").unwrap();
        assert!(stage(&txt, &dir.join("out"), "t").is_err());
        assert!(stage(&dir.join("missing"), &dir.join("out"), "m").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn substitute_replaces_exact_tokens_only() {
        let v = json!({"a": "$attachments.photos", "b": ["$attachments.photos", "x"],
                       "c": "$attachments.other", "d": "see $attachments.photos"});
        let out = substitute(v, &|n| (n == "photos").then(|| json!(["1", "2"])));
        assert_eq!(
            out,
            json!({"a": ["1", "2"], "b": [["1", "2"], "x"],
                               "c": "$attachments.other", "d": "see $attachments.photos"})
        );
    }
}
