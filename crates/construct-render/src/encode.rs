//! Frames as files: PNG for goldens and screenshots, lossless WebP (a RIFF
//! container) for MCP previews.

use image::{ExtendedColorType, ImageEncoder, RgbaImage};

pub fn png(img: &RgbaImage) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(
        &mut out,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Adaptive,
    )
    .write_image(
        img.as_raw(),
        img.width(),
        img.height(),
        ExtendedColorType::Rgba8,
    )
    .expect("encoding to memory");
    out
}

pub fn webp(img: &RgbaImage) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut out)
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            ExtendedColorType::Rgba8,
        )
        .expect("encoding to memory");
    out
}

/// The share of pixels whose largest channel difference exceeds
/// `tolerance` (0..=255): 0.0 is identical. Different sizes count as 1.0.
pub fn diff_ratio(a: &RgbaImage, b: &RgbaImage, tolerance: u8) -> f64 {
    if a.dimensions() != b.dimensions() {
        return 1.0;
    }
    let differing = a
        .pixels()
        .zip(b.pixels())
        .filter(|(p, q)| {
            p.0.iter()
                .zip(q.0.iter())
                .any(|(x, y)| x.abs_diff(*y) > tolerance)
        })
        .count();
    differing as f64 / (a.width() as f64 * a.height() as f64)
}
