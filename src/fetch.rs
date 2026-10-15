//! Outbound HTTP: https-only fetches with deadlines and size limits.

use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures::StreamExt;
use reqwest::redirect;
use tokio::time::Instant;
use url::Url;

/// Build the shared outbound HTTP client.
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        // The fetch standard's redirect limit, which ditto-server inherited.
        .redirect(redirect::Policy::limited(20))
        .connect_timeout(Duration::from_secs(10))
        .build()
        .expect("HTTP client configuration is valid")
}

/// Why a body could not be read.
#[derive(Debug)]
pub enum ReadError {
    TooLarge,
    Network,
}

/// Whether a string is a valid https:// URL.
pub fn is_https_url(url: &str) -> bool {
    Url::parse(url).is_ok_and(|u| u.scheme() == "https")
}

/// Fetch an https URL, following redirects, giving up at `deadline`.
///
/// Returns `None` instead of an error for anything that isn't https and for
/// network failures. The deadline covers reading the body too, so a response
/// handed back by this is also cut off once it passes.
pub async fn safe_fetch(
    client: &reqwest::Client,
    url: &str,
    user_agent: Option<&str>,
    deadline: Instant,
) -> Option<reqwest::Response> {
    if !is_https_url(url) {
        return None;
    }
    let remaining = deadline.checked_duration_since(Instant::now())?;
    let mut req = client.get(url).timeout(remaining);
    if let Some(ua) = user_agent {
        req = req.header(header::USER_AGENT, ua);
    }
    req.send().await.ok()
}

/// Read a response body, failing once it exceeds `max_bytes`.
///
/// A declared `Content-Length` over the limit is rejected before anything is
/// read; the limit is still enforced while streaming, since the header can
/// be absent or wrong.
pub async fn limited_bytes(resp: reqwest::Response, max_bytes: u64) -> Result<Vec<u8>, ReadError> {
    if resp.content_length().is_some_and(|len| len > max_bytes) {
        return Err(ReadError::TooLarge);
    }
    let mut body = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ReadError::Network)?;
        if (body.len() + chunk.len()) as u64 > max_bytes {
            return Err(ReadError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Read a response body as UTF-8 text, failing once it exceeds `max_bytes`.
pub async fn limited_text(resp: reqwest::Response, max_bytes: u64) -> Result<String, ReadError> {
    let body = limited_bytes(resp, max_bytes).await?;
    let text = String::from_utf8_lossy(&body);
    Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_string())
}

/// Read a response body as JSON, failing once it exceeds `max_bytes`.
pub async fn limited_json(
    resp: reqwest::Response,
    max_bytes: u64,
) -> Result<serde_json::Value, ReadError> {
    let body = limited_bytes(resp, max_bytes).await?;
    serde_json::from_slice(&body).map_err(|_| ReadError::Network)
}

/// Stream an upstream image back to the client with minimal headers.
///
/// Streaming keeps a proxied image off the heap, but it means
/// `Content-Length` cannot be trusted to bound it: an oversized declared
/// length is rejected up front, and anything that slips past is cut off
/// mid-stream once `max_bytes` have gone by.
pub fn passthrough_image(
    resp: reqwest::Response,
    max_bytes: u64,
    too_large: &'static str,
) -> Response {
    let mut headers = HeaderMap::new();
    if let Some(ct) = resp.headers().get(header::CONTENT_TYPE) {
        headers.insert(header::CONTENT_TYPE, ct.clone());
    }
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=86400"),
    );
    if let Some(cl) = resp.headers().get(header::CONTENT_LENGTH) {
        let size = cl.to_str().ok().and_then(|s| s.trim().parse::<u64>().ok());
        if size.is_some_and(|size| size > max_bytes) {
            return text(StatusCode::BAD_GATEWAY, too_large);
        }
        headers.insert(header::CONTENT_LENGTH, cl.clone());
    }

    let mut total = 0u64;
    let body = resp.bytes_stream().map(move |chunk| {
        let chunk = chunk?;
        total += chunk.len() as u64;
        if total > max_bytes {
            return Err(format!("response exceeded the {max_bytes} byte limit").into());
        }
        Ok::<_, axum::BoxError>(chunk)
    });

    (StatusCode::OK, headers, Body::from_stream(body)).into_response()
}

/// A plain text response.
pub fn text(status: StatusCode, body: &'static str) -> Response {
    (status, body).into_response()
}
