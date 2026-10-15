//! `GET /favicon/{hostname}`: discover and proxy a site's favicon.

use axum::http::{StatusCode, header};
use axum::response::Response;
use scraper::{Html, Selector};
use tokio::time::Instant;
use url::Url;

use crate::config::FetchLimits;
use crate::fetch::{limited_text, passthrough_image, safe_fetch, text};

/// Discover and proxy the favicon for `hostname`.
///
/// Tries the first `<link rel="…icon…">` on the site's front page, then
/// falls back to `/favicon.ico`.
pub async fn handle_favicon(
    client: &reqwest::Client,
    hostname: &str,
    user_agent: &str,
    limits: &FetchLimits,
) -> Response {
    if hostname.is_empty() || hostname.contains('/') || hostname.contains(':') {
        return text(StatusCode::BAD_REQUEST, "Invalid hostname");
    }

    let deadline = Instant::now() + limits.fetch_timeout;
    let origin = format!("https://{hostname}");
    let ua = Some(user_agent);

    if let Some(href) = extract_icon_href(client, &origin, user_agent, deadline, limits).await
        && let Some(resolved) = resolve_https_url(&href, &origin)
        && let Some(resp) = safe_fetch(client, &resolved, ua, deadline).await
        && resp.status().is_success()
        && is_image_response(&resp)
    {
        return passthrough_image(resp, limits.max_image_bytes, "Favicon too large");
    }

    let fallback = format!("{origin}/favicon.ico");
    if let Some(resp) = safe_fetch(client, &fallback, ua, deadline).await
        && resp.status().is_success()
        && is_image_response(&resp)
    {
        return passthrough_image(resp, limits.max_image_bytes, "Favicon too large");
    }

    text(StatusCode::NOT_FOUND, "Favicon not found")
}

/// Fetch the front page and return the first icon `<link>`'s href.
async fn extract_icon_href(
    client: &reqwest::Client,
    origin: &str,
    user_agent: &str,
    deadline: Instant,
    limits: &FetchLimits,
) -> Option<String> {
    let resp = safe_fetch(client, origin, Some(user_agent), deadline).await?;
    if !resp.status().is_success() {
        return None;
    }
    let html = limited_text(resp, limits.max_html_bytes).await.ok()?;
    first_icon_href(&html)
}

fn first_icon_href(html: &str) -> Option<String> {
    let doc = Html::parse_document(html);
    let links = Selector::parse("link").unwrap();
    doc.select(&links).find_map(|link| {
        let rel = link.value().attr("rel").unwrap_or_default().to_lowercase();
        let href = link.value().attr("href").unwrap_or_default();
        (rel.contains("icon") && !href.is_empty()).then(|| href.to_string())
    })
}

/// Resolve a possibly relative URL against `base`, keeping only https.
fn resolve_https_url(href: &str, base: &str) -> Option<String> {
    let url = Url::parse(base).ok()?.join(href).ok()?;
    (url.scheme() == "https").then(|| url.to_string())
}

fn is_image_response(resp: &reqwest::Response) -> bool {
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    ct.starts_with("image/") || ct.contains("icon") || ct == "application/octet-stream"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_first_icon_link() {
        let html = r#"<html><head>
            <link rel="stylesheet" href="/a.css">
            <link rel="icon" href="">
            <link rel="Shortcut Icon" href="/fav.png">
            <link rel="apple-touch-icon" href="/touch.png">
        </head></html>"#;
        assert_eq!(first_icon_href(html).as_deref(), Some("/fav.png"));
    }

    #[test]
    fn resolves_only_https() {
        let base = "https://example.com";
        assert_eq!(
            resolve_https_url("/f.ico", base).as_deref(),
            Some("https://example.com/f.ico")
        );
        assert_eq!(
            resolve_https_url("//cdn.example.com/f.ico", base).as_deref(),
            Some("https://cdn.example.com/f.ico")
        );
        assert_eq!(resolve_https_url("http://example.com/f.ico", base), None);
        assert_eq!(resolve_https_url("data:image/png;base64,AA", base), None);
    }
}
