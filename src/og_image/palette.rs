//! Color Moments (kind 3367): parsing and the palette OG image SVG.

use std::sync::LazyLock;

use regex::Regex;

use super::relay::Event;
use super::theme::{HEIGHT as H, WIDTH as W};
use crate::js::num;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Horizontal,
    Vertical,
    Grid,
    Star,
    Checkerboard,
    DiagonalStripes,
}

impl Layout {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "horizontal" => Self::Horizontal,
            "vertical" => Self::Vertical,
            "grid" => Self::Grid,
            "star" => Self::Star,
            "checkerboard" => Self::Checkerboard,
            "diagonalStripes" => Self::DiagonalStripes,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
    pub colors: Vec<String>,
    pub name: Option<String>,
    pub layout: Layout,
    /// Emoji drawn in the middle, if the content is one.
    pub emoji: Option<String>,
}

static HEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^#[0-9A-Fa-f]{6}$").unwrap());
static EMOJI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\p{Emoji}").unwrap());

/// Parse a Color Moment event, or `None` if it has no valid colors.
pub fn parse_palette_event(event: &Event) -> Option<Palette> {
    let colors: Vec<String> = event
        .tags
        .iter()
        .filter(|t| t.first().is_some_and(|n| n == "c"))
        .filter_map(|t| t.get(1))
        .filter(|v| HEX.is_match(v))
        .cloned()
        .collect();
    if colors.is_empty() {
        return None;
    }

    let layout = event
        .tag_value("layout")
        .and_then(Layout::parse)
        .unwrap_or(Layout::Horizontal);

    // The content is an emoji overlay only if it's one or two code points.
    let content = event.content.trim();
    let len = content.chars().count();
    let emoji = (len > 0 && len <= 2 && EMOJI.is_match(content)).then(|| content.to_string());

    Some(Palette {
        colors,
        name: event.tag_value("name").map(str::to_string),
        layout,
        emoji,
    })
}

fn rect(x: f64, y: f64, w: f64, h: f64, fill: &str) -> String {
    format!(
        r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{fill}" />"#,
        num(x),
        num(y),
        num(w),
        num(h)
    )
}

fn horizontal(colors: &[String]) -> String {
    let band = H / colors.len() as f64;
    let rects: Vec<_> = colors
        .iter()
        .enumerate()
        .map(|(i, c)| rect(0.0, i as f64 * band, W, band, c))
        .collect();
    rects.join("\n")
}

fn vertical(colors: &[String]) -> String {
    let band = W / colors.len() as f64;
    let rects: Vec<_> = colors
        .iter()
        .enumerate()
        .map(|(i, c)| rect(i as f64 * band, 0.0, band, H, c))
        .collect();
    rects.join("\n")
}

fn grid(colors: &[String]) -> String {
    let n = colors.len();
    let cols = match n {
        ..=3 => n,
        4 => 2,
        _ => 3,
    };
    let rows = n.div_ceil(cols);
    let (cell_w, cell_h) = (W / cols as f64, H / rows as f64);
    let rects: Vec<_> = colors
        .iter()
        .enumerate()
        .map(|(i, c)| {
            rect(
                (i % cols) as f64 * cell_w,
                (i / cols) as f64 * cell_h,
                cell_w,
                cell_h,
                c,
            )
        })
        .collect();
    rects.join("\n")
}

fn star(colors: &[String]) -> String {
    let (cx, cy) = (W / 2.0, H / 2.0);
    let n = colors.len() as f64;
    // Background fill to cover the center seam.
    let mut svg = rect(0.0, 0.0, W, H, &colors[0]);

    for (i, color) in colors.iter().enumerate() {
        let angle = 360.0 / n;
        let overlap = 0.5;
        let start = i as f64 * angle - 90.0 - overlap;
        let sweep = angle + overlap * 2.0;
        let scale = 1.5;
        let steps = 12;

        let mut points = vec![format!("{},{}", num(cx), num(cy))];
        for j in 0..=steps {
            let rad = (start + sweep * j as f64 / steps as f64) * std::f64::consts::PI / 180.0;
            let x = cx + (W / 2.0) * scale * rad.cos();
            let y = cy + (H / 2.0) * scale * rad.sin();
            points.push(format!("{},{}", num(x), num(y)));
        }
        svg.push_str(&format!(
            "\n<polygon points=\"{}\" fill=\"{color}\" />",
            points.join(" ")
        ));
    }
    svg
}

fn checkerboard(colors: &[String]) -> String {
    let n = colors.len();
    let rows = n * 2.max(4usize.saturating_sub(n));
    let cell = H / rows as f64;
    let cols = (W / cell).ceil() as usize;
    let mut rects = Vec::with_capacity(rows * cols);
    for row in 0..rows {
        for col in 0..cols {
            rects.push(rect(
                col as f64 * cell,
                row as f64 * cell,
                cell,
                cell,
                &colors[(row + col) % n],
            ));
        }
    }
    rects.join("\n")
}

fn diagonal_stripes(colors: &[String]) -> String {
    let stripe = (W + H) / colors.len() as f64;
    let polys: Vec<_> = colors
        .iter()
        .enumerate()
        .map(|(i, color)| {
            let (tx0, tx1) = (i as f64 * stripe, (i + 1) as f64 * stripe);
            let (bx0, bx1) = (tx0 - H, tx1 - H);
            format!(
                r#"<polygon points="{},0 {},0 {},{h} {},{h}" fill="{color}" />"#,
                num(tx0),
                num(tx1),
                num(bx1),
                num(bx0),
                h = num(H)
            )
        })
        .collect();
    polys.join("\n")
}

/// An SVG previewing a palette, with the image at `emoji_href` (a rendered
/// emoji) in the middle if given.
pub fn render_palette_svg(palette: &Palette, emoji_href: Option<&str>) -> String {
    let content = match palette.layout {
        Layout::Horizontal => horizontal(&palette.colors),
        Layout::Vertical => vertical(&palette.colors),
        Layout::Grid => grid(&palette.colors),
        Layout::Star => star(&palette.colors),
        Layout::Checkerboard => checkerboard(&palette.colors),
        Layout::DiagonalStripes => diagonal_stripes(&palette.colors),
    };

    let size = 120.0;
    let emoji = emoji_href.map_or(String::new(), |href| {
        format!(
            r#"<image href="{href}" x="{}" y="{}" width="{s}" height="{s}" />"#,
            num(W / 2.0 - size / 2.0),
            num(H / 2.0 - size / 2.0),
            s = num(size)
        )
    });

    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 {w} {h}" width="{w}" height="{h}">
  {content}
  {emoji}
</svg>"#,
        w = num(W),
        h = num(H)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(tags: &[&[&str]], content: &str) -> Event {
        Event {
            id: "id".into(),
            pubkey: "pk".into(),
            created_at: 0,
            kind: 3367,
            tags: tags
                .iter()
                .map(|t| t.iter().map(|s| s.to_string()).collect())
                .collect(),
            content: content.into(),
        }
    }

    #[test]
    fn parses_palettes() {
        let p = parse_palette_event(&event(
            &[
                &["c", "#FF0000"],
                &["c", "red"],
                &["c", "#00ff00"],
                &["name", "Xmas"],
                &["layout", "star"],
            ],
            " 🎄 ",
        ))
        .unwrap();
        assert_eq!(p.colors, ["#FF0000", "#00ff00"]);
        assert_eq!(p.name.as_deref(), Some("Xmas"));
        assert_eq!(p.layout, Layout::Star);
        assert_eq!(p.emoji.as_deref(), Some("🎄"));
    }

    #[test]
    fn ignores_bad_layouts_and_long_content() {
        let p = parse_palette_event(&event(&[&["c", "#000000"], &["layout", "spiral"]], "hello"))
            .unwrap();
        assert_eq!(p.layout, Layout::Horizontal);
        assert_eq!(p.emoji, None);
        assert!(parse_palette_event(&event(&[&["c", "nope"]], "")).is_none());
    }

    #[test]
    fn renders_every_layout() {
        let colors = vec!["#ff0000".to_string(), "#00ff00".into(), "#0000ff".into()];
        for layout in [
            Layout::Horizontal,
            Layout::Vertical,
            Layout::Grid,
            Layout::Star,
            Layout::Checkerboard,
            Layout::DiagonalStripes,
        ] {
            let p = Palette {
                colors: colors.clone(),
                name: None,
                layout,
                emoji: None,
            };
            let svg = render_palette_svg(&p, None);
            assert!(svg.contains("#0000ff"), "{layout:?}");
            assert!(!svg.contains("<image"));
        }
        let p = Palette {
            colors,
            name: None,
            layout: Layout::Horizontal,
            emoji: None,
        };
        assert!(render_palette_svg(&p, Some("e")).contains(r#"<image href="e" x="540" y="255""#));
        assert!(
            render_palette_svg(&p, None)
                .contains(r#"<rect x="0" y="210" width="1200" height="210""#)
        );
    }
}
