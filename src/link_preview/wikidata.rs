//! Wikidata entity URLs don't serve HTML with OG tags (`/entity/` 303s to a
//! JSON blob), so previews for them come from the action API instead.

use std::sync::LazyLock;

use percent_encoding::percent_decode_str;
use regex::Regex;
use serde_json::Value;
use tokio::time::Instant;
use url::Url;

use super::oembed::OEmbed;
use crate::config::FetchLimits;
use crate::fetch::{limited_json, safe_fetch};
use crate::js::encode_uri_component;

static HOST: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(^|\.)wikidata\.org$").unwrap());
static ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[QPLM]\d+$").unwrap());
static ENTITY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^/entity/([^/]+)/?$").unwrap());
static ENTITY_DATA: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^/wiki/Special:EntityData/([^/]+?)(?:\.[a-z]+)?/?$").unwrap()
});
static WIKI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^/wiki/([^/]+)/?$").unwrap());

/// The entity id (eg. "Q12985") of a Wikidata entity URL.
///
/// Recognizes `/entity/{Id}`, `/wiki/{Id}` and
/// `/wiki/Special:EntityData/{Id}(.ext)` on `*.wikidata.org`.
pub fn parse_entity_url(url: &Url) -> Option<String> {
    if !HOST.is_match(url.host_str()?) {
        return None;
    }
    let path = percent_decode_str(url.path()).decode_utf8().ok()?;
    [&*ENTITY, &*ENTITY_DATA, &*WIKI]
        .into_iter()
        .filter_map(|re| re.captures(&path))
        .map(|c| c[1].to_string())
        .find(|id| ID.is_match(id))
}

/// English if there is one, then any English variant, then anything.
fn pick_lang(map: Option<&Value>) -> Option<String> {
    let map = map?.as_object()?;
    let value = |v: &Value| v.get("value").and_then(Value::as_str).map(str::to_string);
    if let Some(en) = map.get("en") {
        return value(en);
    }
    if let Some((_, v)) = map.iter().find(|(k, _)| k.starts_with("en-")) {
        return value(v);
    }
    map.values().next().and_then(value)
}

async fn get_json(
    client: &reqwest::Client,
    url: &str,
    user_agent: &str,
    deadline: Instant,
    limits: &FetchLimits,
) -> Option<Value> {
    let resp = safe_fetch(client, url, Some(user_agent), deadline).await?;
    if !resp.status().is_success() {
        return None;
    }
    limited_json(resp, limits.max_json_bytes).await.ok()
}

/// The Commons file name in an entity's P18 (image) claim.
async fn fetch_image_claim(
    client: &reqwest::Client,
    id: &str,
    user_agent: &str,
    deadline: Instant,
    limits: &FetchLimits,
) -> Option<String> {
    let url = format!(
        "https://www.wikidata.org/w/api.php?action=wbgetclaims&entity={}&property=P18&format=json",
        encode_uri_component(id)
    );
    let data = get_json(client, &url, user_agent, deadline, limits).await?;
    let value = data.pointer("/claims/P18/0/mainsnak/datavalue")?;
    if value.get("type")?.as_str()? != "string" {
        return None;
    }
    value.get("value")?.as_str().map(str::to_string)
}

/// Build a preview for a Wikidata entity, or `None` so the caller can fall
/// back to generic handling.
///
/// Labels come from `wbgetentities` restricted to English, since a full
/// entity document can exceed a megabyte.
pub async fn fetch_preview(
    client: &reqwest::Client,
    id: &str,
    page_url: &Url,
    user_agent: &str,
    deadline: Instant,
    limits: &FetchLimits,
) -> Option<OEmbed> {
    let labels_url = format!(
        "https://www.wikidata.org/w/api.php?action=wbgetentities&ids={}&props=labels%7Cdescriptions&languages=en&format=json",
        encode_uri_component(id)
    );
    let (data, image) = tokio::join!(
        get_json(client, &labels_url, user_agent, deadline, limits),
        fetch_image_claim(client, id, user_agent, deadline, limits),
    );

    let entity = data?.get("entities")?.get(id)?.clone();
    let label = pick_lang(entity.get("labels"));
    let description = pick_lang(entity.get("descriptions"));

    // OEmbed has no description field, so it rides along in the title:
    // "Label (Q123) — description".
    let base = match label {
        Some(label) if !label.is_empty() => format!("{label} ({id})"),
        _ => id.to_string(),
    };
    let title = match description {
        Some(d) if !d.is_empty() => format!("{base} — {d}"),
        _ => base,
    };

    let mut result = OEmbed::new("link");
    result.title = Some(title);
    result.provider_name = Some("Wikidata".into());
    result.provider_url = Some("https://www.wikidata.org".into());
    result.url = Some(page_url.to_string());
    result.thumbnail_url = image.filter(|f| !f.is_empty()).map(|file| {
        format!(
            "https://commons.wikimedia.org/wiki/Special:FilePath/{}?width=1200",
            encode_uri_component(&file)
        )
    });
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_entity_urls() {
        let cases = [
            ("https://www.wikidata.org/entity/Q42", Some("Q42")),
            ("https://www.wikidata.org/wiki/Q42", Some("Q42")),
            ("https://m.wikidata.org/wiki/P31/", Some("P31")),
            (
                "https://www.wikidata.org/wiki/Special:EntityData/Q42.json",
                Some("Q42"),
            ),
            (
                "https://www.wikidata.org/wiki/Special:EntityData/L7",
                Some("L7"),
            ),
            ("https://www.wikidata.org/wiki/Douglas_Adams", None),
            ("https://www.wikidata.org/wiki/Special:Search", None),
            ("https://example.org/wiki/Q42", None),
            ("https://notwikidata.org/wiki/Q42", None),
        ];
        for (url, expected) in cases {
            assert_eq!(
                parse_entity_url(&Url::parse(url).unwrap()).as_deref(),
                expected,
                "{url}"
            );
        }
    }
}
