//! `GET /link-preview/{url}`: an OEmbed-compatible preview of any https page.

pub mod cache;
pub mod oembed;
pub mod page;
pub mod wikidata;

use serde_json::{Map, Value, json};
use tokio::time::Instant;
use url::Url;

use crate::config::FetchLimits;
use crate::fetch::{is_https_url, limited_json, limited_text, safe_fetch};
use oembed::{OEmbed, normalize, resolve_url, synthesize, with_proxied_images};

/// A preview response: the JSON body and its HTTP status.
#[derive(Debug, Clone, PartialEq)]
pub struct Preview {
    pub status: u16,
    pub json: String,
}

impl Preview {
    fn ok(result: OEmbed) -> Self {
        Self {
            status: 200,
            json: serde_json::to_string(&result).unwrap(),
        }
    }

    fn error(status: u16, message: &str) -> Self {
        Self {
            status,
            json: json!({ "error": message }).to_string(),
        }
    }
}

/// Fetch `target_url` and build its preview.
///
/// `api_url` is the base URL of this API. Image URLs in the result point at
/// its `link-preview-image/` endpoint so they are fetched with `user_agent`
/// rather than by the client's browser.
pub async fn fetch_preview(
    client: &reqwest::Client,
    target_url: &str,
    user_agent: &str,
    deadline: Instant,
    limits: &FetchLimits,
    api_url: &str,
) -> Preview {
    let Ok(parsed) = Url::parse(target_url) else {
        return Preview::error(400, "Invalid URL");
    };
    if parsed.scheme() != "https" {
        return Preview::error(400, "URL must be https");
    }

    if let Some(id) = wikidata::parse_entity_url(&parsed)
        && let Some(result) =
            wikidata::fetch_preview(client, &id, &parsed, user_agent, deadline, limits).await
    {
        return Preview::ok(with_proxied_images(result, api_url));
    }

    let Some(page_resp) = safe_fetch(client, target_url, Some(user_agent), deadline)
        .await
        .filter(|r| r.status().is_success())
    else {
        return Preview::error(404, "Could not fetch URL");
    };

    // Relative references resolve against the final URL, which differs from
    // the requested one if a redirect was followed.
    let base = page_resp.url().clone();
    let link_header = page_resp
        .headers()
        .get(reqwest::header::LINK)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    let Ok(html) = limited_text(page_resp, limits.max_html_bytes).await else {
        return Preview::error(502, "Response too large");
    };
    let mut meta = page::extract(&html);

    if let Some(link) = page::parse_oembed_link_header(link_header.as_deref())
        && meta.oembed_json_url.is_none()
    {
        if link.json.is_some() {
            meta.oembed_json_url = link.json;
        }
        if link.xml.is_some() && meta.oembed_xml_url.is_none() {
            meta.oembed_xml_url = link.xml;
        }
    }

    let endpoint = meta
        .oembed_json_url
        .as_ref()
        .or(meta.oembed_xml_url.as_ref())
        .and_then(|e| resolve_url(e, &base))
        .filter(|e| is_https_url(e));
    if let Some(endpoint) = endpoint
        && let Some(native) =
            fetch_native_oembed(client, &endpoint, user_agent, deadline, limits).await
    {
        let result = normalize(&native, &meta, &parsed, &base);
        return Preview::ok(with_proxied_images(result, api_url));
    }

    Preview::ok(with_proxied_images(
        synthesize(&meta, &parsed, &base),
        api_url,
    ))
}

/// `url.searchParams.set(key, value)`, which reserializes the whole query.
fn set_search_param(url: &mut Url, key: &str, value: &str) {
    let mut pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
    match pairs.iter().position(|(k, _)| k == key) {
        Some(i) => {
            pairs[i].1 = value.to_string();
            let mut seen = false;
            pairs.retain(|(k, _)| k != key || !std::mem::replace(&mut seen, true));
        }
        None => pairs.push((key.to_string(), value.to_string())),
    }
    url.query_pairs_mut().clear().extend_pairs(pairs);
}

/// Fetch a native OEmbed endpoint, asking for JSON.
async fn fetch_native_oembed(
    client: &reqwest::Client,
    endpoint: &str,
    user_agent: &str,
    deadline: Instant,
    limits: &FetchLimits,
) -> Option<Map<String, Value>> {
    let mut url = endpoint.to_string();
    if let Ok(mut u) = Url::parse(endpoint)
        && !u.query_pairs().any(|(k, _)| k == "format")
    {
        set_search_param(&mut u, "format", "json");
        url = u.to_string();
    }

    let resp = safe_fetch(client, &url, Some(user_agent), deadline)
        .await
        .filter(|r| r.status().is_success())?;
    let ct = content_type(&resp);

    if ct.contains("json") || ct.contains("javascript") {
        return limited_json(resp, limits.max_json_bytes)
            .await
            .ok()
            .map(as_object);
    }

    // XML came back even though the endpoint already named a format: ask for
    // JSON explicitly.
    if ct.contains("xml") && url == endpoint {
        let mut u = Url::parse(endpoint).ok()?;
        set_search_param(&mut u, "format", "json");
        let resp = safe_fetch(client, u.as_str(), Some(user_agent), deadline)
            .await
            .filter(|r| r.status().is_success())?;
        return limited_json(resp, limits.max_json_bytes)
            .await
            .ok()
            .map(as_object);
    }

    None
}

fn as_object(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn content_type(resp: &reqwest::Response) -> String {
    resp.headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn limits() -> FetchLimits {
        FetchLimits {
            fetch_timeout: Duration::from_secs(1),
            max_html_bytes: 1024,
            max_image_bytes: 1024,
            max_json_bytes: 1024,
        }
    }

    async fn preview(url: &str) -> Preview {
        let deadline = Instant::now() + Duration::from_secs(1);
        fetch_preview(
            &reqwest::Client::new(),
            url,
            "ua",
            deadline,
            &limits(),
            "https://api.example",
        )
        .await
    }

    #[tokio::test]
    async fn rejects_invalid_and_non_https_urls() {
        assert_eq!(
            preview("not a url").await,
            Preview::error(400, "Invalid URL")
        );
        assert_eq!(
            preview("http://example.com").await,
            Preview::error(400, "URL must be https")
        );
        assert_eq!(
            preview("ftp://example.com").await,
            Preview::error(400, "URL must be https")
        );
    }

    #[test]
    fn sets_search_params_like_url_search_params() {
        let mut u =
            Url::parse("https://a.example/o?url=https://b.example/x&format=xml&format=y").unwrap();
        set_search_param(&mut u, "format", "json");
        assert_eq!(
            u.as_str(),
            "https://a.example/o?url=https%3A%2F%2Fb.example%2Fx&format=json"
        );

        let mut u = Url::parse("https://a.example/o?url=x").unwrap();
        set_search_param(&mut u, "format", "json");
        assert_eq!(u.as_str(), "https://a.example/o?url=x&format=json");
    }
}
