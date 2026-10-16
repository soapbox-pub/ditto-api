//! Ditto's API server: link previews and favicons.

pub mod config;
pub mod db;
pub mod favicon;
pub mod fetch;
pub mod gate;
pub mod js;
pub mod link_preview;
pub mod link_preview_image;

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use percent_encoding::percent_decode_str;
use tower_http::cors::{AllowHeaders, Any, CorsLayer};
use tower_http::set_header::SetResponseHeaderLayer;

use config::Config;
use db::Db;
use fetch::text;
use link_preview::cache::{self, LinkPreviewCache};

/// Everything the handlers share.
pub struct AppState {
    pub config: Config,
    pub client: reqwest::Client,
    pub link_previews: LinkPreviewCache,
}

impl AppState {
    pub fn new(config: Config, db: Db) -> Arc<Self> {
        let client = fetch::client();

        let fetch_fn: cache::FetchFn = {
            let client = client.clone();
            let user_agent = config.link_preview_user_agent.clone();
            let limits = config.limits.clone();
            let api_url = config.api_url.clone();
            Arc::new(move |url, deadline| {
                let (client, ua, limits, api_url) = (
                    client.clone(),
                    user_agent.clone(),
                    limits.clone(),
                    api_url.clone(),
                );
                Box::pin(async move {
                    link_preview::fetch_preview(&client, &url, &ua, deadline, &limits, &api_url)
                        .await
                })
            })
        };
        let link_previews = LinkPreviewCache::new(
            db,
            cache::Options {
                ttl: config.link_preview_cache_ttl,
                error_ttl: config.link_preview_error_ttl,
                concurrency: config.link_preview_concurrency,
                queue_limit: config.link_preview_queue_limit,
            },
            &config.limits,
            fetch_fn,
        );

        Arc::new(Self {
            config,
            client,
            link_previews,
        })
    }
}

/// The routes, served both bare and under `/api`.
///
/// ditto-server's namespace was `/api/*`, and clients still use both.
pub fn app(state: Arc<AppState>) -> Router {
    // What Hono's `cors()` defaults to.
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([
            Method::GET,
            Method::HEAD,
            Method::PUT,
            Method::POST,
            Method::DELETE,
            Method::PATCH,
        ])
        .allow_headers(AllowHeaders::mirror_request());

    let routes = Router::new()
        .route("/favicon/{hostname}", get(favicon))
        .route("/link-preview/", get(link_preview))
        .route("/link-preview/{*url}", get(link_preview))
        .route("/link-preview-image/", get(link_preview_image))
        .route("/link-preview-image/{*url}", get(link_preview_image))
        .layer(cors);

    Router::new()
        .merge(routes.clone())
        .nest("/api", routes)
        .fallback(|| async { text(StatusCode::NOT_FOUND, "Not Found") })
        // Nothing here is a page. The favicon and link-preview proxies return
        // bytes fetched from arbitrary sites, and this keeps one of them from
        // being sniffed as a document that runs script on this origin.
        .layer(SetResponseHeaderLayer::overriding(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("default-src 'none'"),
        ))
        .with_state(state)
}

/// The URL wrapped in a `link-preview/` style path: everything after
/// `/{route}/`, percent-decoded once, plus the request's own query string.
///
/// The query belongs to the wrapped URL. Clients put it there unencoded,
/// and caches are keyed on the result, so this must stay exactly as it is.
fn wrapped_url(uri: &Uri, route: &str) -> Option<String> {
    let marker = format!("/{route}/");
    let path = uri.path();
    let encoded = &path[path.find(&marker)? + marker.len()..];
    let mut url = percent_decode_str(encoded).decode_utf8().ok()?.into_owned();
    if let Some(q) = uri.query().filter(|q| !q.is_empty()) {
        url.push('?');
        url.push_str(q);
    }
    Some(url)
}

async fn favicon(State(state): State<Arc<AppState>>, Path(hostname): Path<String>) -> Response {
    let ua = state.config.favicon_user_agent();
    favicon::handle_favicon(&state.client, &hostname, &ua, &state.config.limits).await
}

async fn link_preview(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let Some(url) = wrapped_url(&uri, "link-preview") else {
        return text(StatusCode::BAD_REQUEST, "Invalid URL");
    };
    let r = state.link_previews.fetch(&url).await;
    let status = StatusCode::from_u16(r.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut resp = (
        status,
        [
            (header::CONTENT_TYPE, "application/json".to_string()),
            (header::CACHE_CONTROL, r.cache_control),
        ],
        r.json,
    )
        .into_response();
    if status == StatusCode::SERVICE_UNAVAILABLE {
        resp.headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
    }
    resp
}

async fn link_preview_image(State(state): State<Arc<AppState>>, uri: Uri) -> Response {
    let Some(url) = wrapped_url(&uri, "link-preview-image") else {
        return text(StatusCode::BAD_REQUEST, "Invalid URL");
    };
    let ua = &state.config.link_preview_user_agent;
    link_preview_image::handle(&state.client, &url, ua, &state.config.limits).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwraps_urls_from_paths() {
        let uri: Uri = "/api/link-preview/https%3A%2F%2Fa.example%2Fp%3Fx%3D1?y=2&z=%41"
            .parse()
            .unwrap();
        assert_eq!(
            wrapped_url(&uri, "link-preview").as_deref(),
            Some("https://a.example/p?x=1?y=2&z=%41")
        );
        let uri: Uri = "/link-preview/https://a.example/a%20b/".parse().unwrap();
        assert_eq!(
            wrapped_url(&uri, "link-preview").as_deref(),
            Some("https://a.example/a b/")
        );
        let uri: Uri = "/link-preview/".parse().unwrap();
        assert_eq!(wrapped_url(&uri, "link-preview").as_deref(), Some(""));
    }

    #[tokio::test]
    async fn every_response_forbids_everything() {
        use axum::body::Body;
        use axum::http::Request;
        use tower::ServiceExt;

        let config =
            Config::from_lookup(|key| (key == "PUBLIC_URL").then(|| "https://ditto.pub".into()))
                .unwrap();
        let app = app(AppState::new(config, Db::memory()));
        let requests = [
            Request::get("/nowhere").body(Body::empty()).unwrap(),
            Request::options("/api/favicon/example.com")
                .header(header::ORIGIN, "https://ditto.pub")
                .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
                .body(Body::empty())
                .unwrap(),
        ];
        for request in requests {
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(
                response.headers()[header::CONTENT_SECURITY_POLICY],
                "default-src 'none'"
            );
        }
    }
}
