//! Thumbnails of sent images, for the message bubble.
//!
//! A bubble shows an image at most 240 by 180 CSS pixels, so loading the whole file (up to 25 MB)
//! for every thumbnail wastes time and memory. [`get`] answers a small copy instead: the image
//! scaled down to fit [`MAX_WIDTH`] by [`MAX_HEIGHT`] (twice the displayed box, so it stays sharp on
//! a dense screen), never scaled up.
//!
//! - **Cache.** The thumbnail is kept in the profile's private `thumbs/` folder, named by the
//!   attachment's id. A stored image never changes (its id is generated when it is stored), so the
//!   id is a complete key. The first request makes the file; later ones, including after a restart,
//!   read it. The files go with the chat ([`crate::delete`]).
//! - **Format.** PNG when the picture has transparency, JPEG otherwise. The format is read back
//!   from the file's first bytes, so the cache holds nothing but the image.
//! - **Small or unreadable images** are answered as they are: one already within the box (a small
//!   icon, a tiny GIF) gains nothing from a copy, and one the decoder cannot read still shows
//!   whatever the webview can make of it.
//! - The orientation recorded in a photo's metadata is applied, so the thumbnail matches how the
//!   full image is drawn.

use helm_core::privfs;
use image::{
    codecs::jpeg::JpegEncoder, codecs::png::PngEncoder, DynamicImage, ExtendedColorType,
    ImageDecoder, ImageEncoder, ImageFormat, ImageReader, Limits,
};
use std::io::Cursor;
use std::path::Path;

/// The largest thumbnail, in image pixels.
pub const MAX_WIDTH: u32 = 480;
pub const MAX_HEIGHT: u32 = 360;
/// JPEG quality of a thumbnail.
const JPEG_QUALITY: u8 = 82;
/// The decoder may not allocate more than this for one image (a 20-megapixel image is 80 MB).
const MAX_DECODE_BYTES: u64 = 512 * 1024 * 1024;

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// The type of a cached thumbnail, read from its bytes.
fn sniff(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(PNG_SIGNATURE) {
        "image/png"
    } else {
        "image/jpeg"
    }
}

fn format_of(mime: &str) -> Option<ImageFormat> {
    match mime {
        "image/png" => Some(ImageFormat::Png),
        "image/jpeg" => Some(ImageFormat::Jpeg),
        "image/gif" => Some(ImageFormat::Gif),
        "image/webp" => Some(ImageFormat::WebP),
        _ => None,
    }
}

/// A thumbnail of `bytes` (an image of type `mime`): its type and bytes, or `None` when the image
/// is already within the thumbnail box or cannot be decoded.
pub fn make(mime: &str, bytes: &[u8]) -> Option<(&'static str, Vec<u8>)> {
    let format = format_of(mime)?;
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().ok()?;
    let (w, h) = decoder.dimensions();
    let orientation = decoder.orientation().ok();
    if w <= MAX_WIDTH
        && h <= MAX_HEIGHT
        && orientation.is_none_or(|o| o == image::metadata::Orientation::NoTransforms)
    {
        return None;
    }
    let mut img = DynamicImage::from_decoder(decoder).ok()?;
    if let Some(o) = orientation {
        img.apply_orientation(o);
    }
    let small = if img.width() > MAX_WIDTH || img.height() > MAX_HEIGHT {
        img.thumbnail(MAX_WIDTH, MAX_HEIGHT)
    } else {
        img
    };
    let mut out = Vec::new();
    if small.color().has_alpha() {
        let rgba = small.to_rgba8();
        PngEncoder::new(&mut out)
            .write_image(
                rgba.as_raw(),
                rgba.width(),
                rgba.height(),
                ExtendedColorType::Rgba8,
            )
            .ok()?;
        Some(("image/png", out))
    } else {
        let rgb = small.to_rgb8();
        JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY)
            .write_image(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                ExtendedColorType::Rgb8,
            )
            .ok()?;
        Some(("image/jpeg", out))
    }
}

/// The thumbnail of the stored image `id`. `attachments` and `thumbs` are the profile's two
/// folders. The cached copy is used when there is one; otherwise it is made from the stored file and
/// cached. An image that needs no thumbnail, or that cannot be decoded, is answered as stored
/// (`None` when its file cannot be read at all).
pub fn get(attachments: &Path, thumbs: &Path, id: &str, mime: &str) -> Option<(String, Vec<u8>)> {
    let cached = thumbs.join(id);
    if let Ok(bytes) = std::fs::read(&cached) {
        if !bytes.is_empty() {
            return Some((sniff(&bytes).to_owned(), bytes));
        }
    }
    let original = match std::fs::read(attachments.join(id)) {
        Ok(b) => b,
        Err(e) => {
            log::warn!("attachment {id} could not be read: {e}");
            return None;
        }
    };
    let Some((thumb_mime, thumb)) = make(mime, &original) else {
        return Some((mime.to_owned(), original));
    };
    // A failed cache write costs only the next request making it again.
    if let Err(e) =
        privfs::create_private_dir_all(thumbs).and_then(|_| privfs::write_atomic(&cached, &thumb))
    {
        log::warn!("thumbnail {id} could not be cached: {e}");
    }
    Some((thumb_mime.to_owned(), thumb))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb, Rgba};

    /// A photo-like picture: noise a PNG cannot squeeze much.
    fn png_rgb(w: u32, h: u32) -> Vec<u8> {
        let img = ImageBuffer::from_fn(w, h, |x, y| {
            let n = x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503).rotate_left(7);
            Rgb([(n >> 3) as u8, (n >> 11) as u8, (n >> 19) as u8])
        });
        let mut out = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(img)
            .write_to(&mut out, ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    fn png_rgba(w: u32, h: u32) -> Vec<u8> {
        let img = ImageBuffer::from_fn(w, h, |x, _| Rgba([200, 30, 30, (x % 256) as u8]));
        let mut out = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(img)
            .write_to(&mut out, ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    fn jpeg(w: u32, h: u32) -> Vec<u8> {
        let img = ImageBuffer::from_fn(w, h, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, 40]));
        let mut out = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(img)
            .write_to(&mut out, ImageFormat::Jpeg)
            .unwrap();
        out.into_inner()
    }

    fn size_of(bytes: &[u8]) -> (u32, u32) {
        let r = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .unwrap();
        r.into_dimensions().unwrap()
    }

    #[test]
    fn a_large_image_becomes_a_small_one_that_keeps_its_shape() {
        let big = png_rgb(1920, 1080);
        let (mime, thumb) = make("image/png", &big).expect("a thumbnail");
        assert_eq!(mime, "image/jpeg", "an opaque picture becomes a JPEG");
        let (w, h) = size_of(&thumb);
        assert!(w <= MAX_WIDTH && h <= MAX_HEIGHT, "{w}x{h} fits the box");
        assert!(w == MAX_WIDTH || h == MAX_HEIGHT, "{w}x{h} fills it");
        assert_eq!(w * 1080 / 1920, h, "the aspect ratio is kept");
        assert!(thumb.len() < big.len() / 4, "and it is much smaller");
        assert_eq!(sniff(&thumb), "image/jpeg");
    }

    #[test]
    fn transparency_is_kept_as_a_png() {
        let (mime, thumb) = make("image/png", &png_rgba(900, 900)).expect("a thumbnail");
        assert_eq!(mime, "image/png");
        assert_eq!(sniff(&thumb), "image/png");
        assert_eq!(size_of(&thumb), (360, 360));
    }

    #[test]
    fn a_jpeg_is_thumbnailed_too() {
        let (mime, thumb) = make("image/jpeg", &jpeg(1000, 2000)).expect("a thumbnail");
        assert_eq!(mime, "image/jpeg");
        assert_eq!(size_of(&thumb), (180, 360));
    }

    #[test]
    fn an_image_within_the_box_needs_no_thumbnail_and_garbage_makes_none() {
        assert!(make("image/png", &png_rgb(480, 360)).is_none());
        assert!(make("image/png", &png_rgb(64, 64)).is_none());
        assert!(make("image/png", b"not an image at all").is_none());
        assert!(
            make("image/jpeg", &png_rgb(2000, 2000)).is_none(),
            "wrong type"
        );
        assert!(make("image/bmp", &png_rgb(2000, 2000)).is_none());
    }

    #[test]
    fn the_first_request_caches_and_later_ones_use_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let (att, thumbs) = (dir.path().join("attachments"), dir.path().join("thumbs"));
        std::fs::create_dir_all(&att).unwrap();
        let id = "0b5c1c7e-8d2e-4a43-9f5d-1d7c5a2d9a10";
        let big = png_rgb(1600, 1200);
        std::fs::write(att.join(id), &big).unwrap();
        assert!(!thumbs.join(id).exists());
        let (mime, first) = get(&att, &thumbs, id, "image/png").unwrap();
        assert_eq!(mime, "image/jpeg");
        assert!(first.len() < big.len());
        assert_eq!(std::fs::read(thumbs.join(id)).unwrap(), first, "cached");
        // The cached copy answers without the original (here it is gone).
        std::fs::remove_file(att.join(id)).unwrap();
        let (mime, second) = get(&att, &thumbs, id, "image/png").unwrap();
        assert_eq!((mime.as_str(), second), ("image/jpeg", first));
    }

    #[test]
    fn small_and_unreadable_images_are_answered_as_stored() {
        let dir = tempfile::tempdir().unwrap();
        let (att, thumbs) = (dir.path().join("attachments"), dir.path().join("thumbs"));
        std::fs::create_dir_all(&att).unwrap();
        let small = png_rgb(40, 30);
        std::fs::write(att.join("small"), &small).unwrap();
        assert_eq!(
            get(&att, &thumbs, "small", "image/png"),
            Some(("image/png".to_owned(), small))
        );
        std::fs::write(att.join("junk"), b"junk").unwrap();
        assert_eq!(
            get(&att, &thumbs, "junk", "image/png"),
            Some(("image/png".to_owned(), b"junk".to_vec()))
        );
        assert!(!thumbs.exists(), "neither is cached");
        assert_eq!(get(&att, &thumbs, "missing", "image/png"), None);
    }
}
