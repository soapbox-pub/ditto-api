//! The OEmbed response shape, and building one from a native endpoint's
//! payload or from scraped page metadata.

use serde::Serialize;
use serde_json::{Map, Number, Value};
use url::Url;

use super::page::PageMeta;
use crate::fetch::is_https_url;
use crate::js::{encode_uri_component, escape_xml, or_chain, parse_int};

#[derive(Debug, Clone, Default, Serialize)]
pub struct OEmbed {
    pub version: &'static str,
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_age: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail_width: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail_height: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
}

impl OEmbed {
    pub fn new(kind: &'static str) -> Self {
        Self {
            version: "1.0",
            kind,
            ..Default::default()
        }
    }
}

/// Build the URL of this API's image proxy for `image_url`.
///
/// The target is percent-encoded into a single path segment, mirroring the
/// shape of the `link-preview/` route.
pub fn proxy_image_url(image_url: &str, api_url: &str) -> String {
    format!(
        "{}/link-preview-image/{}",
        api_url.trim_end_matches('/'),
        encode_uri_component(image_url)
    )
}

/// Point a preview's image URLs at this API's image proxy.
///
/// Sites that vary their preview image on the User-Agent (Reddit's
/// `share.redd.it` being the motivating case) hand a browser a generic
/// placeholder while giving a crawler the real thing. Scraping the page as a
/// crawler is not enough, because the client is what ends up requesting the
/// image; routing it back through here means the same User-Agent that saw
/// the real image is the one that fetches it. Only https URLs are rewritten,
/// since the proxy declines anything else.
pub fn with_proxied_images(mut result: OEmbed, api_url: &str) -> OEmbed {
    if let Some(thumb) = result.thumbnail_url.as_mut().filter(|u| is_https_url(u)) {
        *thumb = proxy_image_url(thumb, api_url);
    }
    // For a photo embed `url` is the full-size image itself.
    if result.kind == "photo"
        && let Some(url) = result.url.as_mut().filter(|u| is_https_url(u))
    {
        *url = proxy_image_url(url, api_url);
    }
    result
}

/// Resolve a URL reference against the page it was scraped from.
///
/// Relative and protocol-relative references are resolved rather than
/// discarded. Anything that doesn't resolve to http(s), like `data:` or
/// `javascript:`, yields `None`.
pub fn resolve_url(value: &str, base: &Url) -> Option<String> {
    if value.is_empty() {
        return None;
    }
    let u = base.join(value).ok()?;
    matches!(u.scheme(), "http" | "https").then(|| u.to_string())
}

/// `new URL(...).origin` for an http(s) URL.
pub fn origin(url: &Url) -> String {
    url.origin().ascii_serialization()
}

fn infer_type(meta: &PageMeta) -> &'static str {
    let og_type = meta.og_type.as_deref();
    if meta.og_video.is_some()
        || meta.twitter_player.is_some()
        || matches!(og_type, Some("video.other" | "video"))
    {
        return "video";
    }
    if meta.twitter_card.as_deref() == Some("photo") || og_type == Some("image") {
        return "photo";
    }
    "link"
}

fn int(s: &Option<String>) -> Option<Number> {
    s.as_deref().and_then(parse_int).map(Number::from)
}

/// A JSON number, or a string parsed as an integer (`parseInt || undefined`).
fn number_or_int_string(v: Option<&Value>) -> Option<Number> {
    match v? {
        Value::Number(n) => Some(n.clone()),
        Value::String(s) => parse_int(s).map(Number::from),
        _ => None,
    }
}

fn string(raw: &Map<String, Value>, key: &str) -> Option<String> {
    raw.get(key).and_then(Value::as_str).map(str::to_string)
}

fn number(raw: &Map<String, Value>, key: &str) -> Option<Number> {
    raw.get(key).and_then(|v| v.as_number().cloned())
}

/// Image from page meta: `og:image`, else `twitter:image`.
fn meta_image(meta: &PageMeta, base: &Url) -> Option<String> {
    let src = or_chain(&[meta.og_image.as_ref(), meta.twitter_image.as_ref()]).unwrap_or_default();
    resolve_url(&src, base)
}

fn meta_title(meta: &PageMeta) -> Option<String> {
    or_chain(&[
        meta.og_title.as_ref(),
        meta.twitter_title.as_ref(),
        meta.title.as_ref(),
    ])
}

/// Normalize a native OEmbed payload, filling gaps from page meta.
///
/// `page_url` is the URL that was requested; `base` is what relative
/// references in the payload and page resolve against.
pub fn normalize(raw: &Map<String, Value>, meta: &PageMeta, page_url: &Url, base: &Url) -> OEmbed {
    let kind = match raw.get("type").and_then(Value::as_str) {
        Some("photo") => "photo",
        Some("video") => "video",
        Some("link") => "link",
        Some("rich") => "rich",
        _ => infer_type(meta),
    };
    let mut r = OEmbed::new(kind);

    r.title = string(raw, "title");
    r.author_name = string(raw, "author_name");
    r.author_url = string(raw, "author_url").and_then(|u| resolve_url(&u, base));
    r.provider_name = string(raw, "provider_name");
    r.provider_url = string(raw, "provider_url").and_then(|u| resolve_url(&u, base));
    r.cache_age = number_or_int_string(raw.get("cache_age"));

    r.thumbnail_url = string(raw, "thumbnail_url").and_then(|u| resolve_url(&u, base));
    if r.thumbnail_url.is_some() {
        r.thumbnail_width = number(raw, "thumbnail_width");
        r.thumbnail_height = number(raw, "thumbnail_height");
    }

    r.url = string(raw, "url").and_then(|u| resolve_url(&u, base));
    r.html = string(raw, "html");
    r.width = number_or_int_string(raw.get("width"));
    r.height = number_or_int_string(raw.get("height"));

    if r.title.as_deref().is_none_or(str::is_empty) {
        r.title = meta_title(meta);
    }
    if r.provider_name.as_deref().is_none_or(str::is_empty) {
        r.provider_name = Some(
            meta.og_site_name
                .clone()
                .unwrap_or_else(|| page_url.host_str().unwrap_or_default().to_string()),
        );
    }
    if r.provider_url.is_none() {
        r.provider_url = Some(origin(page_url));
    }
    if r.thumbnail_url.is_none()
        && let Some(image) = meta_image(meta, base)
    {
        r.thumbnail_url = Some(image);
        r.thumbnail_width = int(&meta.og_image_width);
        r.thumbnail_height = int(&meta.og_image_height);
    }

    r
}

/// Synthesize an OEmbed response purely from scraped page metadata.
pub fn synthesize(meta: &PageMeta, page_url: &Url, base: &Url) -> OEmbed {
    let kind = infer_type(meta);
    let mut r = OEmbed::new(kind);
    r.title = meta_title(meta);
    r.provider_name = Some(
        meta.og_site_name
            .clone()
            .unwrap_or_else(|| page_url.host_str().unwrap_or_default().to_string()),
    );
    r.provider_url = Some(origin(page_url));
    r.author_name = meta.author_name.clone();

    let image = meta_image(meta, base);
    if let Some(image) = &image {
        r.thumbnail_url = Some(image.clone());
        r.thumbnail_width = int(&meta.og_image_width);
        r.thumbnail_height = int(&meta.og_image_height);
    }

    if kind == "video" {
        let src =
            or_chain(&[meta.og_video.as_ref(), meta.twitter_player.as_ref()]).unwrap_or_default();
        if let Some(video) = resolve_url(&src, base) {
            let dim = |a: &Option<String>, b: &Option<String>, default: i64| {
                or_chain(&[a.as_ref(), b.as_ref()])
                    .and_then(|s| parse_int(&s))
                    .unwrap_or(default)
            };
            let w = dim(&meta.og_video_width, &meta.twitter_player_width, 480);
            let h = dim(&meta.og_video_height, &meta.twitter_player_height, 270);
            r.html = Some(format!(
                r#"<iframe width="{w}" height="{h}" src="{}" frameborder="0" allowfullscreen></iframe>"#,
                escape_xml(&video)
            ));
            r.width = Some(w.into());
            r.height = Some(h.into());
        }
    }

    if kind == "photo"
        && let Some(image) = image
    {
        r.url = Some(image);
        r.width = int(&meta.og_image_width);
        r.height = int(&meta.og_image_height);
    }

    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_the_proxy_target_into_one_segment() {
        assert_eq!(
            proxy_image_url("https://i.example/a b.png?x=1", "https://api.ditto.pub/"),
            "https://api.ditto.pub/link-preview-image/https%3A%2F%2Fi.example%2Fa%20b.png%3Fx%3D1"
        );
    }

    #[test]
    fn resolves_references_against_the_page() {
        let base = Url::parse("https://example.com/posts/1").unwrap();
        let cases = [
            (
                "https://cdn.example.com/a.png",
                Some("https://cdn.example.com/a.png"),
            ),
            ("/img/a.png", Some("https://example.com/img/a.png")),
            ("a.png", Some("https://example.com/posts/a.png")),
            (
                "//cdn.example.com/a.png",
                Some("https://cdn.example.com/a.png"),
            ),
            ("http://example.com/a.png", Some("http://example.com/a.png")),
            ("data:image/png;base64,AA", None),
            ("javascript:alert(1)", None),
            ("", None),
        ];
        for (value, expected) in cases {
            assert_eq!(resolve_url(value, &base).as_deref(), expected, "{value}");
        }
    }

    #[test]
    fn proxies_https_thumbnails_and_photo_urls_only() {
        let mut r = OEmbed::new("photo");
        r.thumbnail_url = Some("https://i.example/t.png".into());
        r.url = Some("http://i.example/full.png".into());
        let r = with_proxied_images(r, "https://api.ditto.pub");
        assert!(
            r.thumbnail_url
                .unwrap()
                .starts_with("https://api.ditto.pub/link-preview-image/")
        );
        assert_eq!(r.url.as_deref(), Some("http://i.example/full.png"));
    }

    #[test]
    fn synthesizes_a_video_embed() {
        let meta = PageMeta {
            og_title: Some("Clip".into()),
            og_video: Some("/v.mp4".into()),
            og_video_width: Some("640".into()),
            ..Default::default()
        };
        let page = Url::parse("https://example.com/watch").unwrap();
        let r = synthesize(&meta, &page, &page);
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["type"], "video");
        assert_eq!(json["width"], 640);
        assert_eq!(json["height"], 270);
        assert_eq!(json["provider_name"], "example.com");
        assert_eq!(json["provider_url"], "https://example.com");
        assert!(
            json["html"]
                .as_str()
                .unwrap()
                .contains("https://example.com/v.mp4")
        );
    }
}
