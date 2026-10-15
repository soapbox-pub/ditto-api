//! `GET /link-preview-image/{url}`: proxy a link preview's image.
//!
//! Preview images are proxied rather than linked because some sites vary the
//! image on the User-Agent. Reddit's `share.redd.it/preview/post/*` serves
//! the real post image to a recognized crawler and a generic placeholder to
//! anything that looks like a browser. Proxying also keeps the viewer's IP
//! address from whichever host the image lives on.

use axum::http::{StatusCode, header};
use axum::response::Response;
use tokio::time::Instant;
use url::Url;

use crate::config::FetchLimits;
use crate::fetch::{passthrough_image, safe_fetch, text};

pub async fn handle(
    client: &reqwest::Client,
    target_url: &str,
    user_agent: &str,
    limits: &FetchLimits,
) -> Response {
    let Ok(parsed) = Url::parse(target_url) else {
        return text(StatusCode::BAD_REQUEST, "Invalid URL");
    };
    if parsed.scheme() != "https" {
        return text(StatusCode::BAD_REQUEST, "URL must be https");
    }

    let deadline = Instant::now() + limits.fetch_timeout;
    let Some(resp) = safe_fetch(client, target_url, Some(user_agent), deadline)
        .await
        .filter(|r| r.status().is_success())
    else {
        return text(StatusCode::NOT_FOUND, "Could not fetch image");
    };

    // Only images get proxied, or this would relay arbitrary https content
    // under the API's origin.
    let is_image = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.to_ascii_lowercase().starts_with("image/"));
    if !is_image {
        return text(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Not an image");
    }

    passthrough_image(resp, limits.max_image_bytes, "Image too large")
}
