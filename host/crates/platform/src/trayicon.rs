//! The tray icon's state dot: the base icon's pixels with a small filled dot in the bottom-right
//! corner, red when something needs the user, amber while something runs, none when idle.
//!
//! The pixels are composed here, at run time, from whatever icon the app already has, so there is
//! no second set of image files to keep in step. The dot never animates: the icon only changes
//! when the state does.

/// Red: something needs the user.
pub const RED: [u8; 4] = [0xe5, 0x48, 0x4d, 0xff];
/// Amber: something is running.
pub const AMBER: [u8; 4] = [0xf5, 0xa5, 0x24, 0xff];
/// The dot is 4 pixels across on a 16-pixel icon and grows with the icon, so it stays a dot at
/// the sizes Windows scales the icon from.
pub const DOT_PER_16: u32 = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrayDot {
    #[default]
    None,
    Amber,
    Red,
}

impl TrayDot {
    pub fn colour(self) -> Option<[u8; 4]> {
        match self {
            Self::None => None,
            Self::Amber => Some(AMBER),
            Self::Red => Some(RED),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Amber => "amber",
            Self::Red => "red",
        }
    }
}

/// The dot's centre and radius, in pixels from the icon's top-left corner (the dot touches the
/// right and bottom edges). The centre is the middle of a pixel, so the test is exact.
pub fn dot_geometry(width: u32, height: u32) -> (f32, f32, f32) {
    let side = width.min(height).max(1);
    let diameter = (side * DOT_PER_16 / 16).max(DOT_PER_16);
    let r = diameter as f32 / 2.0;
    (width as f32 - r, height as f32 - r, r)
}

/// `base` (RGBA, `width * height * 4` bytes) with the dot for `dot` drawn over it. With no dot, or
/// a buffer of the wrong size, the pixels come back as they were.
pub fn compose(base: &[u8], width: u32, height: u32, dot: TrayDot) -> Vec<u8> {
    let mut px = base.to_vec();
    let Some(c) = dot.colour() else { return px };
    if width == 0 || height == 0 || px.len() != (width as usize) * (height as usize) * 4 {
        return px;
    }
    let (cx, cy, r) = dot_geometry(width, height);
    for y in 0..height {
        for x in 0..width {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            if dx * dx + dy * dy <= r * r {
                let i = ((y * width + x) * 4) as usize;
                px[i..i + 4].copy_from_slice(&c);
            }
        }
    }
    px
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(px: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    }

    #[test]
    fn the_dot_sits_in_the_bottom_right_corner_in_the_state_colour() {
        let base = vec![0x10u8; 16 * 16 * 4];
        let red = compose(&base, 16, 16, TrayDot::Red);
        let amber = compose(&base, 16, 16, TrayDot::Amber);
        // A 4 px dot touching the right and bottom edges: its middle is inside, its far corners
        // are not, and the rest of the icon is untouched.
        assert_eq!(dot_geometry(16, 16), (14.0, 14.0, 2.0));
        assert_eq!(at(&red, 16, 14, 14), RED);
        assert_eq!(at(&red, 16, 13, 13), RED);
        assert_eq!(at(&red, 16, 15, 15), RED);
        assert_eq!(at(&amber, 16, 14, 14), AMBER);
        assert_eq!(at(&red, 16, 12, 12), [0x10; 4], "outside the dot");
        assert_eq!(at(&red, 16, 0, 0), [0x10; 4]);
        assert_eq!(
            at(&red, 16, 15, 12),
            [0x10; 4],
            "the dot is round, not a square"
        );
        let changed = (0..16 * 16)
            .filter(|i| red[i * 4..i * 4 + 4] != base[i * 4..i * 4 + 4])
            .count();
        assert!(
            (10..=16).contains(&changed),
            "a 4 px dot is about a dozen pixels: {changed}"
        );
    }

    #[test]
    fn idle_and_wrong_sized_buffers_come_back_unchanged() {
        let base = vec![7u8; 8 * 8 * 4];
        assert_eq!(compose(&base, 8, 8, TrayDot::None), base);
        assert_eq!(
            compose(&base, 8, 9, TrayDot::Red),
            base,
            "size does not match the pixels"
        );
        assert_eq!(compose(&[], 0, 0, TrayDot::Red), Vec::<u8>::new());
    }

    #[test]
    fn the_dot_grows_with_the_icon() {
        assert_eq!(dot_geometry(16, 16).2, 2.0);
        assert_eq!(dot_geometry(32, 32).2, 4.0);
        assert_eq!(dot_geometry(256, 256).2, 32.0);
        let (cx, cy, r) = dot_geometry(32, 32);
        assert_eq!((cx, cy), (28.0, 28.0));
        let big = compose(&vec![0u8; 32 * 32 * 4], 32, 32, TrayDot::Red);
        assert_eq!(at(&big, 32, 28, 28), RED);
        assert_eq!(
            at(&big, 32, 31, 31),
            [0, 0, 0, 0],
            "the corner itself is outside a round dot of radius {r}"
        );
    }
}
