//! User avatar processing.
//!
//! Uploaded images are normalized to a square, downscaled, and re-encoded as
//! **lossless WebP** (pure-Rust `image` encoder — no libwebp/C dependency). To
//! honour a per-file size budget without a lossy quality knob, we pick the
//! *largest* candidate edge whose encoded size fits the budget (busy photos land
//! on a smaller edge; simple/flat images keep the full edge).
//!
//! Storage is the SQLite `user_avatars` table (a BLOB per user), not the data
//! volume — so avatars are Litestream-replicated with the rest of the DB and any
//! replica can serve any avatar. The upload/serve wiring lives in `main.rs`;
//! this module only turns bytes into budgeted WebP.

use anyhow::{anyhow, bail, Context, Result};
use image::codecs::webp::WebPEncoder;
use image::{imageops, ExtendedColorType, GenericImageView, RgbaImage};

/// Reject raw uploads larger than this before we even decode them — a decode
/// bomb guard and a bound on the work a single request can trigger.
pub const MAX_UPLOAD_BYTES: usize = 8 * 1024 * 1024;

/// Per-avatar size budget for the stored WebP (the "60–70 KB per pic" cap). We
/// keep the largest square that encodes at or under this.
pub const MAX_AVATAR_BYTES: usize = 70 * 1024;

/// Candidate square edge lengths (px), largest first. The first that encodes
/// within [`MAX_AVATAR_BYTES`] wins; if none do, the smallest is used (a 96px
/// lossless avatar is comfortably tiny, so this only trades some sharpness).
const CANDIDATE_EDGES: [u32; 7] = [256, 224, 192, 160, 128, 112, 96];

/// Decode arbitrary uploaded image bytes (JPEG/PNG/WebP), normalize to a square,
/// and re-encode as budgeted lossless WebP. Returns the WebP bytes.
pub fn process_avatar(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.is_empty() {
        bail!("empty upload");
    }
    if bytes.len() > MAX_UPLOAD_BYTES {
        bail!(
            "image too large (max {} MB)",
            MAX_UPLOAD_BYTES / (1024 * 1024)
        );
    }
    let img = image::load_from_memory(bytes).context("unsupported or corrupt image")?;
    let square = to_square(&img);

    let mut smallest: Option<Vec<u8>> = None;
    for edge in CANDIDATE_EDGES {
        let resized = imageops::resize(&square, edge, edge, imageops::FilterType::Lanczos3);
        let encoded = encode_lossless(&resized)?;
        if encoded.len() <= MAX_AVATAR_BYTES {
            return Ok(encoded);
        }
        smallest = Some(encoded); // keep the last (smallest edge) as the fallback
    }
    smallest.ok_or_else(|| anyhow!("no candidate size produced"))
}

/// Center-crop to the largest centered square, as an owned RGBA buffer.
fn to_square(img: &image::DynamicImage) -> RgbaImage {
    let (w, h) = img.dimensions();
    let edge = w.min(h);
    let x = (w - edge) / 2;
    let y = (h - edge) / 2;
    img.crop_imm(x, y, edge, edge).to_rgba8()
}

/// Encode an RGBA buffer as lossless WebP (VP8L) into a byte vector.
fn encode_lossless(img: &RgbaImage) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    WebPEncoder::new_lossless(&mut out)
        .encode(
            img.as_raw(),
            img.width(),
            img.height(),
            ExtendedColorType::Rgba8,
        )
        .context("webp encode failed")?;
    Ok(out)
}

/// The public path stored on the user row for a saved avatar, cache-busted with
/// `?v=<version>` so the browser refetches after a change. The `/avatars/{file}`
/// route reads the bytes from the `user_avatars` table keyed by `<user_id>`.
pub fn avatar_url(user_id: &str, version: i64) -> String {
    format!("/avatars/{user_id}.webp?v={version}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, RgbImage};

    /// A deterministic, high-entropy test image that resists lossless
    /// compression (so the budget logic actually has to downscale for it).
    fn noisy_png(w: u32, h: u32) -> Vec<u8> {
        let mut img = RgbImage::new(w, h);
        for (x, y, px) in img.enumerate_pixels_mut() {
            // A cheap pseudo-random-ish pattern — no repeating runs.
            let r = ((x * 73 + y * 151) % 256) as u8;
            let g = ((x * 199 + y * 37) % 256) as u8;
            let b = ((x ^ (y.wrapping_mul(101))) % 256) as u8;
            *px = image::Rgb([r, g, b]);
        }
        let mut bytes = Vec::new();
        DynamicImage::ImageRgb8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    #[test]
    fn output_is_square_webp_within_budget() {
        // A wide, noisy source: must be cropped square and fit the byte budget.
        let src = noisy_png(600, 400);
        let webp = process_avatar(&src).expect("processes");
        assert!(
            webp.len() <= MAX_AVATAR_BYTES,
            "avatar {} bytes exceeds budget {}",
            webp.len(),
            MAX_AVATAR_BYTES
        );
        let decoded = image::load_from_memory(&webp).expect("valid webp");
        let (w, h) = decoded.dimensions();
        assert_eq!(w, h, "avatar must be square");
        assert!(
            CANDIDATE_EDGES.contains(&w),
            "edge {w} not a candidate size"
        );
    }

    #[test]
    fn simple_image_keeps_full_edge() {
        // A flat image compresses tiny, so the largest candidate edge is kept.
        let flat = {
            let img = RgbImage::from_pixel(500, 500, image::Rgb([20, 120, 200]));
            let mut bytes = Vec::new();
            DynamicImage::ImageRgb8(img)
                .write_to(
                    &mut std::io::Cursor::new(&mut bytes),
                    image::ImageFormat::Png,
                )
                .unwrap();
            bytes
        };
        let webp = process_avatar(&flat).unwrap();
        let (w, _) = image::load_from_memory(&webp).unwrap().dimensions();
        assert_eq!(
            w, CANDIDATE_EDGES[0],
            "flat image should keep the full edge"
        );
    }

    #[test]
    fn rejects_empty_and_oversize() {
        assert!(process_avatar(&[]).is_err());
        assert!(process_avatar(&vec![0u8; MAX_UPLOAD_BYTES + 1]).is_err());
    }

    #[test]
    fn rejects_non_image() {
        assert!(process_avatar(b"this is not an image").is_err());
    }

    #[test]
    fn avatar_url_is_versioned_path() {
        assert_eq!(avatar_url("user-123", 42), "/avatars/user-123.webp?v=42");
    }
}
