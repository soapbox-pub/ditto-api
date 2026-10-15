//! Persistent link preview cache with negative caching, request coalescing
//! and a bound on concurrent outbound fetches.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::time::Instant;

use super::Preview;
use crate::config::FetchLimits;
use crate::db::{Db, LinkPreviewEntry, now_ms};
use crate::gate::{Coalescer, Gate, QueueFull};

/// Fixed-length cache key for a URL.
///
/// Keys are hashed from the URL verbatim, with no normalization, so a
/// trailing slash or a different query order is a different entry.
pub fn cache_key(target_url: &str) -> String {
    hex::encode(Sha256::digest(target_url.as_bytes()))
}

/// Delete the stored preview for a URL, returning whether one existed.
///
/// A fetch already in flight for the URL will still write its result.
pub fn revoke(db: &Db, target_url: &str) -> anyhow::Result<bool> {
    db.remove_link_preview(&cache_key(target_url))
}

/// Fetches a preview from the network.
pub type FetchFn =
    Arc<dyn Fn(String, Instant) -> Pin<Box<dyn Future<Output = Preview> + Send>> + Send + Sync>;

pub struct Options {
    /// How long a successful preview stays fresh.
    pub ttl: Duration,
    /// How long a failure stays fresh.
    pub error_ttl: Duration,
    /// Maximum concurrent outbound fetches.
    pub concurrency: usize,
    /// Maximum fetches queued behind those before requests are shed.
    pub queue_limit: usize,
}

/// A response to hand back: status, body, and `Cache-Control`.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedResponse {
    pub status: u16,
    pub json: String,
    pub cache_control: String,
}

pub struct LinkPreviewCache {
    db: Db,
    opts: Options,
    fetch_fn: FetchFn,
    fetch_timeout: Duration,
    gate: Arc<Gate>,
    in_flight: Coalescer<String, Result<LinkPreviewEntry, QueueFull>>,
}

/// Whether a status is worth storing.
///
/// A 400 is decided from the URL alone, before any outbound request, so
/// remembering it would spend disk to save nothing. Every other status was
/// paid for with a network round trip.
fn is_cacheable(status: u16) -> bool {
    status != 400
}

impl LinkPreviewCache {
    pub fn new(db: Db, opts: Options, limits: &FetchLimits, fetch_fn: FetchFn) -> Self {
        Self {
            gate: Arc::new(Gate::new(opts.concurrency, opts.queue_limit)),
            db,
            opts,
            fetch_fn,
            fetch_timeout: limits.fetch_timeout,
            in_flight: Coalescer::new(),
        }
    }

    /// Look up the preview for a URL.
    ///
    /// A fresh entry is returned as is. A stale one is returned too, with a
    /// refresh started in the background. With nothing stored, the caller
    /// waits for a fetch.
    pub async fn fetch(&self, target_url: &str) -> CachedResponse {
        let key = cache_key(target_url);
        let entry = self.db.get_link_preview(&key).unwrap_or_else(|e| {
            tracing::error!("link preview cache read failed: {e:#}");
            None
        });

        if let Some(entry) = entry {
            let age = now_ms() - entry.fetched_at;
            if age < self.ttl_for(entry.status).as_millis() as i64 {
                return self.response(&entry);
            }
            // The refresh is spawned, so dropping the handle doesn't cancel
            // it. Whether it succeeds is not this caller's problem.
            drop(self.refresh(key, target_url));
            return self.response(&entry);
        }

        match self.refresh(key, target_url).await {
            Ok(entry) => self.response(&entry),
            Err(QueueFull) => busy(),
        }
    }

    fn ttl_for(&self, status: u16) -> Duration {
        if status == 200 {
            self.opts.ttl
        } else {
            self.opts.error_ttl
        }
    }

    /// Fetch and store the preview, at most once at a time per URL.
    ///
    /// Without coalescing, a burst of requests for one uncached URL becomes a
    /// burst of identical outbound fetches, and a stale entry under load
    /// spawns an unbounded number of background refreshes.
    fn refresh(
        &self,
        key: String,
        target_url: &str,
    ) -> impl Future<Output = Result<LinkPreviewEntry, QueueFull>> + use<> {
        let db = self.db.clone();
        let gate = self.gate.clone();
        let fetch_fn = self.fetch_fn.clone();
        let timeout = self.fetch_timeout;
        let target_url = target_url.to_string();
        let task_key = key.clone();

        self.in_flight.run(key, move || async move {
            gate.run(async move {
                // Shared between callers, so the deadline can't be any one
                // caller's.
                let preview = fetch_fn(target_url, Instant::now() + timeout).await;
                let entry = LinkPreviewEntry {
                    json: preview.json,
                    status: preview.status,
                    fetched_at: now_ms(),
                };
                // Failures are stored too, on a shorter lifetime: a dead link
                // costs a full connection attempt up to the timeout every time
                // it's asked for, so not remembering it makes dead links the
                // most expensive request this endpoint serves.
                if is_cacheable(entry.status)
                    && let Err(e) = db.put_link_preview(&task_key, &entry)
                {
                    tracing::error!("link preview cache write failed: {e:#}");
                }
                entry
            })
            .await
        })
    }

    fn response(&self, entry: &LinkPreviewEntry) -> CachedResponse {
        // Advertise the lifetime the entry actually has.
        let cache_control = if is_cacheable(entry.status) {
            format!("public, max-age={}", self.ttl_for(entry.status).as_secs())
        } else {
            "no-store".to_string()
        };
        CachedResponse {
            status: entry.status,
            json: entry.json.clone(),
            cache_control,
        }
    }
}

/// Response for a request shed because the fetch queue is full.
///
/// A missing preview is a far better outcome than an unbounded queue of
/// outbound fetches holding connections open until everything times out.
fn busy() -> CachedResponse {
    CachedResponse {
        status: 503,
        json: r#"{"error":"Too many concurrent link preview fetches"}"#.to_string(),
        cache_control: "no-store".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn limits() -> FetchLimits {
        FetchLimits {
            fetch_timeout: Duration::from_secs(1),
            max_html_bytes: 0,
            max_image_bytes: 0,
            max_json_bytes: 0,
        }
    }

    /// A fetcher that counts calls and answers `status` after `delay`.
    fn counting(status: u16, delay: Duration) -> (FetchFn, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let f: FetchFn = Arc::new(move |url, _| {
            let c = c.clone();
            Box::pin(async move {
                c.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(delay).await;
                Preview {
                    status,
                    json: format!(r#"{{"url":"{url}"}}"#),
                }
            })
        });
        (f, calls)
    }

    fn cache(
        db: &Db,
        fetch_fn: FetchFn,
        concurrency: usize,
        queue_limit: usize,
    ) -> LinkPreviewCache {
        let opts = Options {
            ttl: Duration::from_secs(86_400),
            error_ttl: Duration::from_secs(600),
            concurrency,
            queue_limit,
        };
        LinkPreviewCache::new(db.clone(), opts, &limits(), fetch_fn)
    }

    #[tokio::test]
    async fn caches_successes_and_failures_but_not_400s() {
        for (status, cached) in [(200, true), (404, true), (502, true), (400, false)] {
            let db = Db::memory();
            let (f, calls) = counting(status, Duration::ZERO);
            let c = cache(&db, f, 8, 64);
            let first = c.fetch("https://a.example/").await;
            c.fetch("https://a.example/").await;
            assert_eq!(first.status, status);
            assert_eq!(
                calls.load(Ordering::SeqCst),
                if cached { 1 } else { 2 },
                "{status}"
            );
        }
    }

    #[tokio::test]
    async fn advertises_each_entrys_lifetime() {
        let db = Db::memory();
        let (ok, _) = counting(200, Duration::ZERO);
        assert_eq!(
            cache(&db, ok, 8, 64)
                .fetch("https://a/")
                .await
                .cache_control,
            "public, max-age=86400"
        );
        let (err, _) = counting(404, Duration::ZERO);
        assert_eq!(
            cache(&db, err, 8, 64)
                .fetch("https://b/")
                .await
                .cache_control,
            "public, max-age=600"
        );
        let (bad, _) = counting(400, Duration::ZERO);
        assert_eq!(
            cache(&db, bad, 8, 64)
                .fetch("https://c/")
                .await
                .cache_control,
            "no-store"
        );
    }

    #[tokio::test]
    async fn ages_failures_out_on_the_error_ttl() {
        let db = Db::memory();
        let (f, calls) = counting(404, Duration::ZERO);
        let c = cache(&db, f, 8, 64);
        let key = cache_key("https://a/");
        let stale = LinkPreviewEntry {
            json: "{}".into(),
            status: 404,
            fetched_at: now_ms() - 601_000,
        };
        db.put_link_preview(&key, &stale).unwrap();

        // Served stale while a background refresh runs.
        assert_eq!(c.fetch("https://a/").await.json, "{}");
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_ne!(db.get_link_preview(&key).unwrap().unwrap().json, "{}");
    }

    #[tokio::test]
    async fn coalesces_concurrent_requests() {
        let db = Db::memory();
        let (f, calls) = counting(200, Duration::from_millis(20));
        let c = cache(&db, f, 8, 64);
        let (a, b) = tokio::join!(c.fetch("https://a/"), c.fetch("https://a/"));
        assert_eq!(a, b);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn sheds_requests_once_the_queue_is_full() {
        let db = Db::memory();
        let (f, _) = counting(200, Duration::from_millis(50));
        let c = cache(&db, f, 1, 0);
        let (a, b) = tokio::join!(c.fetch("https://a/"), async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            c.fetch("https://b/").await
        });
        assert_eq!(a.status, 200);
        assert_eq!(b.status, 503);
        assert_eq!(b.cache_control, "no-store");
    }

    #[tokio::test]
    async fn revoking_makes_the_next_request_refetch() {
        let db = Db::memory();
        let (f, calls) = counting(200, Duration::ZERO);
        let c = cache(&db, f, 8, 64);
        c.fetch("https://a/").await;
        assert!(revoke(&db, "https://a/").unwrap());
        assert!(!revoke(&db, "https://a/").unwrap());
        c.fetch("https://a/").await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
