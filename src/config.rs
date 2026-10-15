use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};

/// Limits applied to every upstream HTTP fetch.
#[derive(Debug, Clone)]
pub struct FetchLimits {
    /// Budget for a whole upstream operation, redirects and body included.
    pub fetch_timeout: Duration,
    /// Maximum size of an HTML page.
    pub max_html_bytes: u64,
    /// Maximum size of an image.
    pub max_image_bytes: u64,
    /// Maximum size of a JSON document (eg. OEmbed).
    pub max_json_bytes: u64,
}

/// Server configuration, read from the environment.
#[derive(Debug, Clone)]
pub struct Config {
    /// Port for the HTTP server.
    pub port: u16,
    /// Public URL of the Ditto frontend (eg. "https://ditto.pub").
    pub public_url: String,
    /// Base URL of this API, path prefix included and without a trailing
    /// slash (eg. "https://api.ditto.pub").
    ///
    /// Every absolute API URL handed back to a client is built from it: the
    /// link preview image proxy URLs baked into link previews.
    pub api_url: String,
    /// Nostr relays queried for OG image data.
    pub relay_urls: Vec<String>,
    /// Timeout for relay queries.
    pub query_timeout: Duration,
    /// User-Agent sent when fetching link previews and their images.
    pub link_preview_user_agent: String,
    /// Limits for upstream HTTP fetches.
    pub limits: FetchLimits,
    /// Path of the SQLite cache database.
    pub db_path: PathBuf,
    /// How long a successful link preview stays fresh.
    pub link_preview_cache_ttl: Duration,
    /// How long a failed link preview stays fresh.
    ///
    /// Much shorter than the success TTL because a failure is often
    /// transient, while still collapsing a relentless crawl of dead links
    /// into one fetch per interval.
    pub link_preview_error_ttl: Duration,
    /// Maximum concurrent outbound link preview fetches.
    pub link_preview_concurrency: usize,
    /// Maximum link preview fetches queued before requests are shed.
    pub link_preview_queue_limit: usize,
    /// How long a rendered OG image stays fresh.
    pub og_image_cache_ttl: Duration,
}

impl Config {
    /// Read the configuration from the process environment.
    pub fn from_env() -> Result<Self> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Read the configuration through an arbitrary lookup function.
    ///
    /// Empty values are treated as unset, matching ditto-server.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let get = |key: &str| lookup(key).filter(|v| !v.is_empty());

        let port = match get("PORT") {
            None => 3000,
            Some(v) => v
                .parse::<u16>()
                .ok()
                .filter(|p| *p >= 1)
                .context("PORT must be a valid port number (1-65535).")?,
        };

        let public_url = get("PUBLIC_URL").context("PUBLIC_URL is required.")?;

        let api_url = match get("API_URL") {
            Some(v) => v.trim_end_matches('/').to_string(),
            None => format!("{}/api", public_url.trim_end_matches('/')),
        };

        let relay_urls = get("RELAY_URLS")
            .or_else(|| get("RELAY_URL"))
            .unwrap_or_else(|| "wss://relay.ditto.pub".to_string())
            .split(',')
            .map(|url| url.trim().to_string())
            .filter(|url| !url.is_empty())
            .collect();

        let ms = |key: &str, default: u64| -> Result<Duration> {
            Ok(Duration::from_millis(non_neg_int(&get, key, default)?))
        };

        Ok(Self {
            port,
            public_url,
            api_url,
            relay_urls,
            query_timeout: ms("QUERY_TIMEOUT", 5_000)?,
            link_preview_user_agent: get("LINK_PREVIEW_USER_AGENT")
                .unwrap_or_else(|| "facebookexternalhit".to_string()),
            limits: FetchLimits {
                fetch_timeout: ms("FETCH_TIMEOUT", 3_000)?,
                max_html_bytes: non_neg_int(&get, "MAX_HTML_BYTES", 2 * 1024 * 1024)?,
                max_image_bytes: non_neg_int(&get, "MAX_IMAGE_BYTES", 2 * 1024 * 1024)?,
                max_json_bytes: non_neg_int(&get, "MAX_JSON_BYTES", 256 * 1024)?,
            },
            db_path: db_path(&get),
            link_preview_cache_ttl: ms("LINK_PREVIEW_CACHE_TTL", 86_400_000)?,
            link_preview_error_ttl: ms("LINK_PREVIEW_ERROR_TTL", 600_000)?,
            link_preview_concurrency: pos_int(&get, "LINK_PREVIEW_CONCURRENCY", 8)? as usize,
            link_preview_queue_limit: non_neg_int(&get, "LINK_PREVIEW_QUEUE_LIMIT", 64)? as usize,
            og_image_cache_ttl: ms("OG_IMAGE_CACHE_TTL", 3_600_000)?,
        })
    }

    /// User-Agent sent when discovering favicons.
    pub fn favicon_user_agent(&self) -> String {
        format!("Ditto <{}>", self.public_url)
    }
}

/// Path of the SQLite cache database, from `DB_PATH`.
///
/// Split out so tools that only touch the cache don't need the rest of the
/// configuration to be valid.
pub fn db_path(get: &impl Fn(&str) -> Option<String>) -> PathBuf {
    get("DB_PATH")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "data/ditto-api.sqlite".to_string())
        .into()
}

fn non_neg_int(get: &impl Fn(&str) -> Option<String>, key: &str, default: u64) -> Result<u64> {
    match get(key) {
        None => Ok(default),
        Some(v) => match v.parse::<u64>() {
            Ok(n) => Ok(n),
            Err(_) => bail!("{key} must be a non-negative integer."),
        },
    }
}

fn pos_int(get: &impl Fn(&str) -> Option<String>, key: &str, default: u64) -> Result<u64> {
    match get(key) {
        None => Ok(default),
        Some(v) => match v.parse::<u64>() {
            Ok(n) if n >= 1 => Ok(n),
            _ => bail!("{key} must be a positive integer."),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(vars: &[(&str, &str)]) -> Result<Config> {
        Config::from_lookup(|key| {
            vars.iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| v.to_string())
        })
    }

    #[test]
    fn requires_public_url() {
        assert!(config(&[]).is_err());
    }

    #[test]
    fn defaults_api_url_to_same_origin_prefix() {
        let c = config(&[("PUBLIC_URL", "https://ditto.pub/")]).unwrap();
        assert_eq!(c.api_url, "https://ditto.pub/api");
    }

    #[test]
    fn strips_trailing_slashes_from_api_url() {
        let c = config(&[
            ("PUBLIC_URL", "https://ditto.pub"),
            ("API_URL", "https://api.ditto.pub//"),
        ])
        .unwrap();
        assert_eq!(c.api_url, "https://api.ditto.pub");
    }

    #[test]
    fn splits_relay_urls() {
        let c = config(&[
            ("PUBLIC_URL", "https://ditto.pub"),
            ("RELAY_URLS", "wss://a.example, ,wss://b.example"),
        ])
        .unwrap();
        assert_eq!(c.relay_urls, ["wss://a.example", "wss://b.example"]);
    }

    #[test]
    fn rejects_bad_numbers() {
        assert!(config(&[("PUBLIC_URL", "x"), ("PORT", "0")]).is_err());
        assert!(config(&[("PUBLIC_URL", "x"), ("FETCH_TIMEOUT", "-1")]).is_err());
        assert!(config(&[("PUBLIC_URL", "x"), ("LINK_PREVIEW_CONCURRENCY", "0")]).is_err());
    }
}
