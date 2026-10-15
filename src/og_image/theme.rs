//! Ditto themes (kinds 16767 and 36767): parsing, color derivation, and the
//! theme and profile OG image SVGs.
//!
//! The color math is ported from the Ditto frontend's `colorUtils.ts`, by
//! way of ditto-server, so generated images match what the app shows. It
//! follows JavaScript's NaN semantics on purpose: malformed colors in an
//! event yield `rgb(NaN,NaN,NaN)`, as they always have.

use serde_json::Value;

use super::relay::Event;
use crate::js::{self, escape_xml, num, round};

/// The three core colors of a theme, as HSL strings like "228 20% 10%".
#[derive(Debug, Clone, PartialEq)]
pub struct CoreColors {
    pub background: String,
    pub text: String,
    pub primary: String,
}

/// Background image configuration from a `bg` tag.
#[derive(Debug, Clone, PartialEq)]
pub struct Background {
    pub url: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub colors: CoreColors,
    pub title: String,
    pub description: Option<String>,
    pub background: Option<Background>,
}

impl Theme {
    /// The theme a profile without one is rendered with.
    pub fn default_profile() -> Self {
        Self {
            colors: CoreColors {
                background: "228 20% 10%".into(),
                text: "210 40% 98%".into(),
                primary: "263 70% 50%".into(),
            },
            title: "Profile".into(),
            description: None,
            background: None,
        }
    }
}

// ─── HSL ───

struct Hsl {
    h: f64,
    s: f64,
    l: f64,
}

fn parse_hsl(hsl: &str) -> Hsl {
    let cleaned = hsl.trim().replace('%', "");
    let parts: Vec<f64> = cleaned.split_whitespace().map(js::to_number).collect();
    // `"".split(/\s+/)` is `[""]`, which is `[0]`.
    let parts = if cleaned.is_empty() { vec![0.0] } else { parts };
    let at = |i: usize| parts.get(i).copied().unwrap_or(f64::NAN);
    Hsl {
        h: at(0),
        s: at(1),
        l: at(2),
    }
}

fn format_hsl(h: f64, s: f64, l: f64) -> String {
    let r = |n: f64| num(round(n * 10.0) / 10.0);
    format!("{} {}% {}%", r(h), r(s), r(l))
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> [f64; 3] {
    let sn = s / 100.0;
    let ln = l / 100.0;
    let k = |n: f64| (n + h / 30.0) % 12.0;
    let a = sn * js::min(ln, 1.0 - ln);
    let f = |n: f64| ln - a * js::max(-1.0, js::min(k(n) - 3.0, js::min(9.0 - k(n), 1.0)));
    [
        round(f(0.0) * 255.0),
        round(f(8.0) * 255.0),
        round(f(4.0) * 255.0),
    ]
}

fn hsl_to_rgb_string(hsl: &str) -> String {
    let Hsl { h, s, l } = parse_hsl(hsl);
    let [r, g, b] = hsl_to_rgb(h, s, l);
    format!("rgb({},{},{})", num(r), num(g), num(b))
}

fn luminance([r, g, b]: [f64; 3]) -> f64 {
    let c = |v: f64| {
        let v = v / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * c(r) + 0.7152 * c(g) + 0.0722 * c(b)
}

fn is_dark(background: &str) -> bool {
    let Hsl { h, s, l } = parse_hsl(background);
    luminance(hsl_to_rgb(h, s, l)) < 0.2
}

fn lighten(hsl: &str, amount: f64) -> String {
    let Hsl { h, s, l } = parse_hsl(hsl);
    format_hsl(h, s, js::min(100.0, l + amount))
}

fn darken(hsl: &str, amount: f64) -> String {
    let Hsl { h, s, l } = parse_hsl(hsl);
    format_hsl(h, s, js::max(0.0, l - amount))
}

/// The derived theme tokens the OG images use.
#[derive(Debug, Clone, PartialEq)]
pub struct Tokens {
    pub background: String,
    pub foreground: String,
    pub card: String,
    pub primary: String,
    pub muted: String,
    pub muted_foreground: String,
}

pub fn derive_tokens(colors: &CoreColors) -> Tokens {
    let dark = is_dark(&colors.background);
    let card = if dark {
        lighten(&colors.background, 2.0)
    } else {
        colors.background.clone()
    };
    let muted = if dark {
        lighten(&colors.background, 8.0)
    } else {
        darken(&colors.background, 4.0)
    };

    let fg = parse_hsl(&colors.text);
    let muted_foreground = if dark {
        format_hsl(fg.h, js::max(fg.s - 20.0, 0.0), js::max(fg.l - 30.0, 40.0))
    } else {
        format_hsl(fg.h, js::max(fg.s - 30.0, 0.0), js::min(fg.l + 35.0, 55.0))
    };

    Tokens {
        background: colors.background.clone(),
        foreground: colors.text.clone(),
        card,
        primary: colors.primary.clone(),
        muted,
        muted_foreground,
    }
}

// ─── Hex → HSL ───

fn hex_to_hsl_string(hex: &str) -> String {
    let mut h: Vec<char> = hex.replacen('#', "", 1).chars().collect();
    if h.len() == 3 {
        h = vec![h[0], h[0], h[1], h[1], h[2], h[2]];
    }
    let byte = |i: usize| {
        let s: String = h.iter().skip(i).take(2).collect();
        js::parse_hex_int(&s) / 255.0
    };
    let (r, g, b) = (byte(0), byte(2), byte(4));

    let max = js::max(r, js::max(g, b));
    let min = js::min(r, js::min(g, b));
    let l = (max + min) / 2.0;
    if max == min {
        return format_hsl(0.0, 0.0, l * 100.0);
    }
    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let hue = if max == r {
        ((g - b) / d + if g < b { 6.0 } else { 0.0 }) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    format_hsl(hue * 360.0, s * 100.0, l * 100.0)
}

// ─── Event parsing ───

/// Colors from `["c", hex, role]` tags; `None` unless all three roles are set.
fn parse_color_tags(tags: &[Vec<String>]) -> Option<CoreColors> {
    let mut roles = std::collections::HashMap::new();
    for tag in tags {
        if let [name, hex, role, ..] = tag.as_slice()
            && name == "c"
            && !hex.is_empty()
            && !role.is_empty()
        {
            roles.insert(role.as_str(), hex.as_str());
        }
    }
    Some(CoreColors {
        background: hex_to_hsl_string(roles.get("background")?),
        text: hex_to_hsl_string(roles.get("text")?),
        primary: hex_to_hsl_string(roles.get("primary")?),
    })
}

/// The background from an imeta-style `bg` tag.
fn parse_background_tag(event: &Event) -> Option<Background> {
    let tag = event.tag("bg")?;
    let url = tag
        .iter()
        .skip(1)
        .filter_map(|entry| entry.split_once(' '))
        .rfind(|(k, _)| *k == "url")?
        .1;
    (!url.is_empty()).then(|| Background {
        url: url.to_string(),
    })
}

/// JavaScript truthiness of a JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|n| n != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// `String(value)` for a JSON value.
fn js_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Colors from the legacy JSON-in-content format, current or 4-color.
fn parse_legacy_colors(content: &str) -> Option<CoreColors> {
    let parsed: Value = serde_json::from_str(content).ok()?;
    let get = |k| parsed.get(k);
    let text_key = if truthy(get("text")) {
        "text"
    } else {
        "foreground"
    };
    if truthy(get("background")) && truthy(get(text_key)) && truthy(get("primary")) {
        return Some(CoreColors {
            background: js_string(&parsed["background"]),
            text: js_string(&parsed[text_key]),
            primary: js_string(&parsed["primary"]),
        });
    }
    None
}

/// Parse a theme definition (36767) or active profile theme (16767).
pub fn parse_theme_event(event: &Event) -> Option<Theme> {
    let colors = parse_color_tags(&event.tags).or_else(|| {
        (!event.content.is_empty())
            .then(|| parse_legacy_colors(&event.content))
            .flatten()
    })?;

    let title = event
        .tag_value("title")
        .map(str::to_string)
        .unwrap_or_else(|| {
            if event.kind == 16767 {
                "Profile Theme"
            } else {
                "Theme"
            }
            .into()
        });

    Some(Theme {
        colors,
        title,
        description: event.tag_value("description").map(str::to_string),
        background: parse_background_tag(event),
    })
}

// ─── SVG ───

pub const WIDTH: f64 = 1200.0;
pub const HEIGHT: f64 = 630.0;
const CONTENT_PADDING: f64 = 32.0;

fn background_image(theme: &Theme) -> String {
    match &theme.background {
        Some(bg) => format!(
            r#"<image href="{}" x="0" y="0" width="{W}" height="{H}" preserveAspectRatio="xMidYMid slice" opacity="0.4" />"#,
            escape_xml(&bg.url),
            W = num(WIDTH),
            H = num(HEIGHT),
        ),
        None => String::new(),
    }
}

/// An SVG previewing a theme's colors and background.
pub fn render_theme_svg(theme: &Theme) -> String {
    let tokens = derive_tokens(&theme.colors);
    let bg = hsl_to_rgb_string(&tokens.background);
    let fg = hsl_to_rgb_string(&tokens.foreground);
    let muted_fg = hsl_to_rgb_string(&tokens.muted_foreground);
    let primary = hsl_to_rgb_string(&tokens.primary);

    let line1_y = 80.0;
    let line2_y = line1_y + CONTENT_PADDING + 16.0;
    let button_y = line2_y + CONTENT_PADDING + 8.0;
    let (w, h, p) = (num(WIDTH), num(HEIGHT), num(CONTENT_PADDING));

    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 {w} {h}" width="{w}" height="{h}">
  <!-- Background color -->
  <rect x="0" y="0" width="{w}" height="{h}" fill="{bg}" />
  {bg_image}

  <!-- Content skeleton -->
  <rect x="{p}" y="{y1}" width="{w1}" height="16" rx="8" fill="{fg}" opacity="0.6" />
  <rect x="{p}" y="{y2}" width="{w2}" height="16" rx="8" fill="{muted_fg}" opacity="0.4" />

  <!-- Button -->
  <rect x="{p}" y="{yb}" width="160" height="36" rx="6" fill="{primary}" />
</svg>"#,
        bg_image = background_image(theme),
        y1 = num(line1_y),
        y2 = num(line2_y),
        yb = num(button_y),
        w1 = num(WIDTH * 0.45),
        w2 = num(WIDTH * 0.3),
    )
}

/// What a profile OG image is drawn from.
#[derive(Debug, Clone)]
pub struct ProfileData {
    pub theme: Theme,
    pub avatar_url: Option<String>,
    /// Emoji from the kind 0 `shape` field. When set, the avatar is masked
    /// with its silhouette instead of a circle.
    pub shape: Option<String>,
}

/// An SVG of a profile's avatar on its theme's background.
///
/// `mask_href` points at a luminance mask of the emoji shape, if there is
/// one; otherwise the avatar is clipped to a circle.
pub fn render_profile_svg(data: &ProfileData, mask_href: Option<&str>) -> String {
    let tokens = derive_tokens(&data.theme.colors);
    let bg = hsl_to_rgb_string(&tokens.background);
    let muted = hsl_to_rgb_string(&tokens.muted);

    let size = 400.0;
    let (cx, cy) = (WIDTH / 2.0, HEIGHT / 2.0);
    let (ax, ay) = (num(cx - size / 2.0), num(cy - size / 2.0));
    let (cx, cy, r, size) = (num(cx), num(cy), num(size / 2.0), num(size));

    let mask_defs = |href: &str| {
        [
            "<defs>".to_string(),
            r#"  <mask id="avatar-mask">"#.to_string(),
            format!(
                r#"    <image href="{href}" x="{ax}" y="{ay}" width="{size}" height="{size}"/>"#
            ),
            "  </mask>".to_string(),
            "</defs>".to_string(),
        ]
    };

    let avatar = match (&data.avatar_url, mask_href) {
        (None, Some(mask)) => {
            let mut lines = mask_defs(mask).to_vec();
            lines.push(format!(
                r#"<rect x="{ax}" y="{ay}" width="{size}" height="{size}" fill="{muted}" mask="url(#avatar-mask)"/>"#
            ));
            lines.join("\n  ")
        }
        (None, None) => format!(r#"<circle cx="{cx}" cy="{cy}" r="{r}" fill="{muted}" />"#),
        (Some(url), Some(mask)) => {
            let mut lines = mask_defs(mask).to_vec();
            lines.push(format!(
                r#"<image href="{}" x="{ax}" y="{ay}" width="{size}" height="{size}" preserveAspectRatio="xMidYMid slice" mask="url(#avatar-mask)"/>"#,
                escape_xml(url)
            ));
            lines.join("\n  ")
        }
        (Some(url), None) => [
            "<defs>".to_string(),
            r#"  <clipPath id="avatar-clip">"#.to_string(),
            format!(r#"    <circle cx="{cx}" cy="{cy}" r="{r}" />"#),
            "  </clipPath>".to_string(),
            "</defs>".to_string(),
            format!(
                r#"<image href="{}" x="{ax}" y="{ay}" width="{size}" height="{size}" preserveAspectRatio="xMidYMid slice" clip-path="url(#avatar-clip)"/>"#,
                escape_xml(url)
            ),
        ]
        .join("\n  "),
    };

    let (w, h) = (num(WIDTH), num(HEIGHT));
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 {w} {h}" width="{w}" height="{h}">
  <!-- Background color -->
  <rect x="0" y="0" width="{w}" height="{h}" fill="{bg}" />
  {bg_image}

  <!-- Avatar -->
  {avatar}
</svg>"#,
        bg_image = background_image(&data.theme),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn event(kind: u64, tags: &[&[&str]], content: &str) -> Event {
        Event {
            id: "id".into(),
            pubkey: "pk".into(),
            created_at: 0,
            kind,
            tags: tags
                .iter()
                .map(|t| t.iter().map(|s| s.to_string()).collect())
                .collect(),
            content: content.into(),
        }
    }

    #[test]
    fn derives_dark_and_light_tokens() {
        let dark = CoreColors {
            background: "228 20% 10%".into(),
            text: "210 40% 98%".into(),
            primary: "258 70% 60%".into(),
        };
        let t = derive_tokens(&dark);
        assert_eq!(t.card, "228 20% 12%");
        assert_eq!(t.muted, "228 20% 18%");
        assert_eq!(t.muted_foreground, "210 20% 68%");

        let light = CoreColors {
            background: "270 50% 97%".into(),
            text: "270 25% 12%".into(),
            primary: "270 65% 55%".into(),
        };
        let t = derive_tokens(&light);
        assert_eq!(t.card, light.background);
        assert_eq!(t.muted, "270 50% 93%");
        assert_eq!(t.muted_foreground, "270 0% 47%");
    }

    #[test]
    fn converts_hex_to_hsl() {
        assert_eq!(hex_to_hsl_string("#ff0000"), "0 100% 50%");
        assert_eq!(hex_to_hsl_string("#fff"), "0 0% 100%");
        assert_eq!(hex_to_hsl_string("#1a1d26"), "225 18.8% 12.5%");
        assert_eq!(hex_to_hsl_string("red"), "NaN NaN% NaN%");
    }

    #[test]
    fn formats_rgb_strings() {
        assert_eq!(hsl_to_rgb_string("228 20% 10%"), "rgb(20,22,31)");
        assert_eq!(hsl_to_rgb_string("bogus"), "rgb(NaN,NaN,NaN)");
    }

    #[test]
    fn parses_c_tag_themes() {
        let e = event(
            36767,
            &[
                &["c", "#000000", "background"],
                &["c", "#ffffff", "text"],
                &["c", "#ff0000", "primary"],
                &["title", "Night"],
                &["bg", "url https://img.example/bg.png", "mode cover"],
            ],
            "",
        );
        let theme = parse_theme_event(&e).unwrap();
        assert_eq!(theme.colors.background, "0 0% 0%");
        assert_eq!(theme.title, "Night");
        assert_eq!(theme.background.unwrap().url, "https://img.example/bg.png");
    }

    #[test]
    fn parses_legacy_content_themes() {
        let e = event(
            16767,
            &[],
            r#"{"background":"1 2% 3%","foreground":"4 5% 6%","primary":"7 8% 9%"}"#,
        );
        let theme = parse_theme_event(&e).unwrap();
        assert_eq!(theme.colors.text, "4 5% 6%");
        assert_eq!(theme.title, "Profile Theme");
    }

    #[test]
    fn rejects_incomplete_themes() {
        assert!(parse_theme_event(&event(36767, &[], "")).is_none());
        let partial = event(
            36767,
            &[&["c", "#000000", "background"], &["c", "#ffffff", "text"]],
            "",
        );
        assert!(parse_theme_event(&partial).is_none());
    }

    #[test]
    fn renders_theme_svgs() {
        let mut theme = Theme::default_profile();
        let svg = render_theme_svg(&theme);
        assert!(svg.contains(r#"width="1200" height="630""#));
        assert!(svg.contains(r#"width="540""#));
        assert!(!svg.contains("<image"));

        theme.background = Some(Background {
            url: "https://a.example/x?a=1&b=<2>".into(),
        });
        let svg = render_theme_svg(&theme);
        assert!(svg.contains("https://a.example/x?a=1&amp;b=&lt;2&gt;"));
    }

    #[test]
    fn renders_profile_svgs() {
        let mut data = ProfileData {
            theme: Theme::default_profile(),
            avatar_url: None,
            shape: None,
        };
        assert!(render_profile_svg(&data, None).contains(r#"<circle cx="600" cy="315" r="200""#));
        assert!(render_profile_svg(&data, Some("m")).contains(r#"mask="url(#avatar-mask)""#));

        data.avatar_url = Some("https://a.example/me.png".into());
        assert!(render_profile_svg(&data, None).contains(r#"clip-path="url(#avatar-clip)""#));
        let masked = render_profile_svg(&data, Some("m"));
        assert!(masked.contains(r#"<image href="m" x="400" y="115" width="400" height="400"/>"#));
    }
}
