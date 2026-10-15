//! Emoji rendering from the bundled Noto Color Emoji font, so results are
//! the same on every server regardless of installed fonts.

use std::io::Cursor;
use std::sync::{Arc, LazyLock};

use image::imageops::{FilterType, crop_imm, resize};
use image::{ImageFormat, RgbaImage};
use resvg::tiny_skia::Pixmap;
use resvg::usvg::{self, fontdb};

use crate::js::escape_xml;

static FONT: &[u8] = include_bytes!("../../fonts/NotoColorEmoji.ttf");

/// A font database holding only the emoji font.
pub static FONTDB: LazyLock<Arc<fontdb::Database>> = LazyLock::new(|| {
    let mut db = fontdb::Database::new();
    db.load_font_data(FONT.to_vec());
    Arc::new(db)
});

/// Pixels at or below this alpha don't count as part of the glyph.
const ALPHA_THRESHOLD: u8 = 25;

/// Render `emoji`, crop it to its tight bounding box squared up, and scale
/// it to `size` × `size`. `None` if nothing visible was drawn.
fn render_cropped(emoji: &str, size: u32) -> Option<RgbaImage> {
    // Drawn on a canvas half again as big as the font size, so glyphs wider
    // than they are tall still have room for squaring to expand into.
    let font_size = 512.0;
    let canvas = (font_size * 1.5) as u32;
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{canvas}" height="{canvas}"><text x="{x}" y="{y}" font-family="Noto Color Emoji" font-size="{font_size}" text-anchor="middle">{}</text></svg>"#,
        escape_xml(emoji),
        x = canvas / 2,
        y = canvas as f32 / 2.0 + font_size * 0.38,
    );

    let opts = usvg::Options {
        font_family: "Noto Color Emoji".into(),
        fontdb: FONTDB.clone(),
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(&svg, &opts).ok()?;
    let mut pixmap = Pixmap::new(canvas, canvas)?;
    resvg::render(&tree, Default::default(), &mut pixmap.as_mut());

    let (width, height) = (canvas, canvas);
    let mut img = RgbaImage::new(width, height);
    for (dst, src) in img.pixels_mut().zip(pixmap.pixels()) {
        let c = src.demultiply();
        *dst = image::Rgba([c.red(), c.green(), c.blue(), c.alpha()]);
    }

    let (mut top, mut bottom, mut left, mut right) = (height, 0, width, 0);
    for (x, y, px) in img.enumerate_pixels() {
        if px[3] > ALPHA_THRESHOLD {
            top = top.min(y);
            bottom = bottom.max(y);
            left = left.min(x);
            right = right.max(x);
        }
    }
    if right < left || bottom < top {
        return None;
    }

    // Square the box, centered on the shorter axis.
    let crop_w = right - left + 1;
    let crop_h = bottom - top + 1;
    let side = crop_w.max(crop_h);
    if crop_w < side {
        left = left.saturating_sub((side - crop_w) / 2);
    }
    if crop_h < side {
        top = top.saturating_sub((side - crop_h) / 2);
    }
    let side = side.min(width - left).min(height - top);

    let cropped = crop_imm(&img, left, top, side, side).to_image();
    Some(resize(&cropped, size, size, FilterType::Lanczos3))
}

fn png(img: &RgbaImage) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Png)
        .expect("encoding a PNG in memory");
    out.into_inner()
}

/// A full-color square PNG of `emoji`.
pub fn emoji_png(emoji: &str, size: u32) -> Option<Vec<u8>> {
    render_cropped(emoji, size).map(|img| png(&img))
}

/// A square luminance-mask PNG of `emoji`'s silhouette, for an SVG `<mask>`:
/// white where the emoji is opaque ("show"), black where it's transparent.
pub fn emoji_mask_png(emoji: &str, size: u32) -> Option<Vec<u8>> {
    let mut img = render_cropped(emoji, size)?;
    for px in img.pixels_mut() {
        let a = px[3];
        *px = image::Rgba([a, a, a, 255]);
    }
    Some(png(&img))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_emoji_and_masks() {
        for emoji in ["🔷", "❤️", "⭐", "🌙", "👨‍👩‍👧"] {
            let png = emoji_png(emoji, 128).unwrap_or_else(|| panic!("no image for {emoji}"));
            let img = image::load_from_memory(&png).unwrap().to_rgba8();
            assert_eq!(img.dimensions(), (128, 128));
            // Cropped tight, so the glyph reaches the image's edge.
            let edge = (0..128).flat_map(|i| [(0, i), (127, i), (i, 0), (i, 127)]);
            assert!(
                edge.into_iter().any(|(x, y)| img.get_pixel(x, y)[3] > 0),
                "{emoji}"
            );

            let mask = image::load_from_memory(&emoji_mask_png(emoji, 64).unwrap())
                .unwrap()
                .to_rgba8();
            assert!(
                mask.pixels()
                    .all(|p| p[3] == 255 && p[0] == p[1] && p[1] == p[2])
            );
        }
    }

    #[test]
    fn draws_nothing_for_blank_input() {
        assert!(emoji_png(" ", 64).is_none());
    }
}
