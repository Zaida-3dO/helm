//! Reading just enough of a picture to decide whether Helm can take it: its type from the first
//! bytes (never from what Telegram or the sender claims), its size in pixels from the header, and
//! whether it is animated. Nothing is decoded.
//!
//! The limits are the ones the Helm chat's own attachment check applies, which checks them again
//! on arrival: PNG, JPEG and WebP only, at most 20 megapixels, no animation.

/// The most pixels a picture may have.
pub const MAX_PIXELS: u64 = 20_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Png,
    Jpeg,
    Webp,
    /// Recognised, but not accepted: a GIF is not sent inline.
    Gif,
}

impl Kind {
    pub fn mime(self) -> &'static str {
        match self {
            Kind::Png => "image/png",
            Kind::Jpeg => "image/jpeg",
            Kind::Webp => "image/webp",
            Kind::Gif => "image/gif",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Kind::Png => "png",
            Kind::Jpeg => "jpg",
            Kind::Webp => "webp",
            Kind::Gif => "gif",
        }
    }
}

/// The picture type the bytes start with, or `None` when it is not one of the four.
pub fn sniff(b: &[u8]) -> Option<Kind> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Kind::Png)
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(Kind::Jpeg)
    } else if b.get(0..4) == Some(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        Some(Kind::Webp)
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some(Kind::Gif)
    } else {
        None
    }
}

/// Width and height from the header.
pub fn dimensions(kind: Kind, b: &[u8]) -> Option<(u32, u32)> {
    let be16 = |i: usize| {
        b.get(i..i + 2)
            .map(|s| u16::from_be_bytes([s[0], s[1]]) as u32)
    };
    let be32 = |i: usize| {
        b.get(i..i + 4)
            .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    };
    let le16 = |i: usize| {
        b.get(i..i + 2)
            .map(|s| u16::from_le_bytes([s[0], s[1]]) as u32)
    };
    let le24 = |i: usize| {
        b.get(i..i + 3)
            .map(|s| s[0] as u32 | (s[1] as u32) << 8 | (s[2] as u32) << 16)
    };
    match kind {
        Kind::Png if b.get(12..16) == Some(b"IHDR") => Some((be32(16)?, be32(20)?)),
        Kind::Gif => Some((le16(6)?, le16(8)?)),
        Kind::Webp => match b.get(12..16)? {
            b"VP8X" => Some((le24(24)? + 1, le24(27)? + 1)),
            b"VP8L" => {
                let v = u32::from_le_bytes(b.get(21..25)?.try_into().ok()?);
                Some(((v & 0x3fff) + 1, ((v >> 14) & 0x3fff) + 1))
            }
            b"VP8 " => Some((le16(26)? & 0x3fff, le16(28)? & 0x3fff)),
            _ => None,
        },
        Kind::Jpeg => {
            // Walk the segments to the first start-of-frame marker.
            let mut i = 2;
            while i + 9 < b.len() {
                if b[i] != 0xff {
                    i += 1;
                    continue;
                }
                let marker = b[i + 1];
                if (0xc0..=0xcf).contains(&marker) && ![0xc4, 0xc8, 0xcc].contains(&marker) {
                    return Some((be16(i + 7)?, be16(i + 5)?));
                }
                i += 2 + be16(i + 2)? as usize;
            }
            None
        }
        _ => None,
    }
}

/// An animated PNG (an `acTL` chunk ahead of the image data) or an animated WebP (the animation
/// flag in its extended header). A GIF is refused by type, so it is not asked here.
pub fn is_animated(kind: Kind, b: &[u8]) -> bool {
    match kind {
        Kind::Png => {
            let mut at = 8;
            while at + 8 <= b.len() {
                match &b[at + 4..at + 8] {
                    b"acTL" => return true,
                    b"IDAT" | b"IEND" => return false,
                    _ => {}
                }
                // The length is unsigned; a corrupt one that runs past the end ends the scan.
                let len = u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as usize;
                at = match at.checked_add(12 + len) {
                    Some(next) if next <= b.len() => next,
                    _ => return false,
                };
            }
            false
        }
        Kind::Webp => b.get(12..16) == Some(b"VP8X") && b.get(20).is_some_and(|f| f & 0x02 != 0),
        _ => false,
    }
}
