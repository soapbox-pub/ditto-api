//! Metadata scraped from an HTML page's `<title>`, `<meta>` and `<link>`.

use scraper::{Html, Selector};

#[derive(Debug, Clone, Default)]
pub struct PageMeta {
    pub title: Option<String>,
    pub description: Option<String>,
    pub og_title: Option<String>,
    pub og_description: Option<String>,
    pub og_image: Option<String>,
    pub og_image_width: Option<String>,
    pub og_image_height: Option<String>,
    pub og_type: Option<String>,
    pub og_site_name: Option<String>,
    pub og_url: Option<String>,
    pub og_video: Option<String>,
    pub og_video_width: Option<String>,
    pub og_video_height: Option<String>,
    pub og_video_type: Option<String>,
    pub twitter_card: Option<String>,
    pub twitter_title: Option<String>,
    pub twitter_description: Option<String>,
    pub twitter_image: Option<String>,
    pub twitter_player: Option<String>,
    pub twitter_player_width: Option<String>,
    pub twitter_player_height: Option<String>,
    pub author_name: Option<String>,
    pub oembed_json_url: Option<String>,
    pub oembed_xml_url: Option<String>,
}

/// Extract all useful metadata from an HTML document.
pub fn extract(html: &str) -> PageMeta {
    let doc = Html::parse_document(html);
    let mut meta = PageMeta::default();

    let title = Selector::parse("title").unwrap();
    if let Some(el) = doc.select(&title).next() {
        meta.title = Some(el.text().collect::<String>().trim().to_string());
    }

    let metas = Selector::parse("meta").unwrap();
    for el in doc.select(&metas) {
        let attr = |name| el.value().attr(name).filter(|v| !v.is_empty());
        let property = attr("property")
            .or_else(|| attr("name"))
            .unwrap_or_default()
            .to_lowercase();
        let Some(raw) = attr("content") else { continue };
        let content = raw.to_string();

        let first = |slot: &mut Option<String>| {
            if slot.is_none() {
                *slot = Some(content.clone());
            }
        };
        let last = |slot: &mut Option<String>| *slot = Some(content.clone());

        match property.as_str() {
            "og:title" => last(&mut meta.og_title),
            "og:description" => last(&mut meta.og_description),
            "og:image" => first(&mut meta.og_image),
            "og:image:width" => last(&mut meta.og_image_width),
            "og:image:height" => last(&mut meta.og_image_height),
            "og:type" => last(&mut meta.og_type),
            "og:site_name" => last(&mut meta.og_site_name),
            "og:url" => last(&mut meta.og_url),
            "og:video" | "og:video:url" => first(&mut meta.og_video),
            "og:video:width" => last(&mut meta.og_video_width),
            "og:video:height" => last(&mut meta.og_video_height),
            "og:video:type" => last(&mut meta.og_video_type),
            "twitter:card" => last(&mut meta.twitter_card),
            "twitter:title" => last(&mut meta.twitter_title),
            "twitter:description" => last(&mut meta.twitter_description),
            "twitter:image" => first(&mut meta.twitter_image),
            "twitter:player" => last(&mut meta.twitter_player),
            "twitter:player:width" => last(&mut meta.twitter_player_width),
            "twitter:player:height" => last(&mut meta.twitter_player_height),
            "author" => last(&mut meta.author_name),
            "description" => first(&mut meta.description),
            _ => {}
        }
    }

    // <link rel="alternate"> for OEmbed discovery.
    let links = Selector::parse("link").unwrap();
    for el in doc.select(&links) {
        let attr = |name| el.value().attr(name).unwrap_or_default();
        let href = attr("href");
        if href.is_empty() {
            continue;
        }
        let rel = attr("rel").to_lowercase();
        if rel == "alternate" || rel == "alternative" {
            match attr("type").to_lowercase().as_str() {
                "application/json+oembed" => meta.oembed_json_url = Some(href.to_string()),
                "text/xml+oembed" => meta.oembed_xml_url = Some(href.to_string()),
                _ => {}
            }
        }
    }

    meta
}

/// OEmbed discovery URLs advertised in a `Link` response header.
#[derive(Debug, Default, PartialEq)]
pub struct LinkHeaderOEmbed {
    pub json: Option<String>,
    pub xml: Option<String>,
}

/// Parse a `Link` header for OEmbed discovery URLs.
pub fn parse_oembed_link_header(header: Option<&str>) -> Option<LinkHeaderOEmbed> {
    let header = header?;
    let mut result = LinkHeaderOEmbed::default();
    for part in split_link_header(header) {
        let Some(start) = part.find('<') else {
            continue;
        };
        let Some(len) = part[start + 1..].find('>').filter(|len| *len > 0) else {
            continue;
        };
        let url = &part[start + 1..start + 1 + len];
        if part.contains("application/json+oembed") {
            result.json = Some(url.to_string());
        } else if part.contains("text/xml+oembed") {
            result.xml = Some(url.to_string());
        }
    }
    (result.json.is_some() || result.xml.is_some()).then_some(result)
}

/// Split a `Link` header on the commas that begin a new `<url>` entry, so
/// commas inside parameters stay put.
fn split_link_header(header: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    for (i, c) in header.char_indices() {
        if c == ',' && header[i + 1..].trim_start().starts_with('<') {
            parts.push(&header[start..i]);
            start = i + 1;
        }
    }
    parts.push(&header[start..]);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_meta_tags() {
        let html = r#"<html><head>
            <title> A &amp;amp; B </title>
            <meta property="og:image:width" content="?f=webp&amp;region=us">
            <meta property="og:title" content="OG">
            <meta property="og:image" content="/first.png">
            <meta property="og:image" content="/second.png">
            <meta name="description" content="Desc">
            <meta name="twitter:card" content="photo">
            <meta property="og:site_name" content="">
            <link rel="alternate" type="application/json+oembed" href="/oembed?u=1">
        </head></html>"#;
        let meta = extract(html);
        // Entities are decoded exactly once.
        assert_eq!(meta.title.as_deref(), Some("A &amp; B"));
        assert_eq!(meta.og_image_width.as_deref(), Some("?f=webp&region=us"));
        assert_eq!(meta.og_title.as_deref(), Some("OG"));
        assert_eq!(meta.og_image.as_deref(), Some("/first.png"));
        assert_eq!(meta.description.as_deref(), Some("Desc"));
        assert_eq!(meta.twitter_card.as_deref(), Some("photo"));
        assert_eq!(meta.og_site_name, None);
        assert_eq!(meta.oembed_json_url.as_deref(), Some("/oembed?u=1"));
    }

    #[test]
    fn parses_link_headers() {
        let header = r#"<https://a.example/oembed?f=json>; rel="alternate"; type="application/json+oembed", <https://a.example/oembed?f=xml>; rel="alternate"; type="text/xml+oembed""#;
        assert_eq!(
            parse_oembed_link_header(Some(header)),
            Some(LinkHeaderOEmbed {
                json: Some("https://a.example/oembed?f=json".into()),
                xml: Some("https://a.example/oembed?f=xml".into()),
            })
        );
        assert_eq!(
            parse_oembed_link_header(Some("<https://a.example/>; rel=next")),
            None
        );
        assert_eq!(parse_oembed_link_header(None), None);
    }
}
