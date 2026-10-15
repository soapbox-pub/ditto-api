//! `GET /og-image/{nip19}`: a generated Open Graph image for a Nostr entity.
//!
//! - `npub` / `nprofile`: the avatar on the profile's active theme
//! - `note` / `nevent` / `naddr`: depends on the kind; themes (16767, 36767)
//!   and Color Moments (3367) have renderers, other kinds 404

pub mod emoji;
pub mod palette;
pub mod relay;
pub mod render;
pub mod theme;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use nostr::nips::nip19::{FromBech32, Nip19};
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tokio::time::Instant;

use crate::config::FetchLimits;
use crate::db::{Db, OgImageEntry, now_ms};
use crate::gate::Coalescer;
use palette::Palette;
use relay::{Event, Relays};
use render::Images;
use theme::{ProfileData, Theme};

/// `href`s the renderers use for images generated here rather than fetched.
const EMOJI_HREF: &str = "ditto:emoji";
const MASK_HREF: &str = "ditto:avatar-mask";

/// What to draw.
#[derive(Debug, Clone)]
enum Job {
    Theme(Theme),
    Palette(Palette),
    Profile(ProfileData),
}

impl Job {
    /// The SVG for this job, and any images generated for it. CPU-bound.
    fn build(self) -> (String, Images) {
        let mut images = Images::new();
        let svg = match self {
            Job::Theme(theme) => theme::render_theme_svg(&theme),
            Job::Palette(p) => {
                let emoji = p.emoji.as_deref().and_then(|e| emoji::emoji_png(e, 240));
                let href = emoji.map(|png| {
                    images.insert(EMOJI_HREF.into(), png);
                    EMOJI_HREF
                });
                palette::render_palette_svg(&p, href)
            }
            Job::Profile(data) => {
                let mask = data
                    .shape
                    .as_deref()
                    .and_then(|s| emoji::emoji_mask_png(s, 400));
                let href = mask.map(|png| {
                    images.insert(MASK_HREF.into(), png);
                    MASK_HREF
                });
                theme::render_profile_svg(&data, href)
            }
        };
        (svg, images)
    }
}

/// The render job for an event, if its kind has a renderer.
fn event_job(event: &Event) -> Option<Job> {
    match event.kind {
        16767 | 36767 => theme::parse_theme_event(event).map(Job::Theme),
        3367 => palette::parse_palette_event(event).map(Job::Palette),
        _ => None,
    }
}

/// The render job for a profile: its avatar and shape from kind 0, on its
/// active theme (kind 16767) or the default one.
fn profile_job(events: &[Event]) -> Job {
    let profile = events
        .iter()
        .find(|e| e.kind == 0)
        .and_then(|e| serde_json::from_str::<Value>(&e.content).ok());
    let field = |k| {
        profile
            .as_ref()
            .and_then(|p| p.get(k))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let theme = events
        .iter()
        .find(|e| e.kind == 16767)
        .and_then(theme::parse_theme_event)
        .unwrap_or_else(Theme::default_profile);
    Job::Profile(ProfileData {
        theme,
        avatar_url: field("picture"),
        shape: field("shape"),
    })
}

type RenderResult = Result<Option<Bytes>, Arc<anyhow::Error>>;

/// Renders OG images and caches them in the database.
pub struct OgImages {
    db: Db,
    ttl: Duration,
    relays: Arc<Relays>,
    client: reqwest::Client,
    limits: FetchLimits,
    query_timeout: Duration,
    /// Bounds CPU-bound rendering to the cores spared by the server.
    renders: Arc<Semaphore>,
    in_flight: Coalescer<String, RenderResult>,
}

impl OgImages {
    pub fn new(
        db: Db,
        ttl: Duration,
        relays: Relays,
        client: reqwest::Client,
        limits: FetchLimits,
        query_timeout: Duration,
    ) -> Arc<Self> {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        Arc::new(Self {
            db,
            ttl,
            relays: Arc::new(relays),
            client,
            limits,
            query_timeout,
            renders: Arc::new(Semaphore::new(cores.saturating_sub(1).max(1))),
            in_flight: Coalescer::new(),
        })
    }

    /// The JPEG for a NIP-19 identifier, or `None` if there is nothing to
    /// draw (invalid identifier, event not found, kind without a renderer).
    ///
    /// A fresh cached image is returned as is. A stale one is returned too,
    /// with a re-render started in the background. With nothing cached, the
    /// caller waits for a render.
    pub async fn fetch(self: &Arc<Self>, nip19: &str) -> RenderResult {
        let entry = self.db.get_og_image(nip19).unwrap_or_else(|e| {
            tracing::error!("OG image cache read failed: {e:#}");
            None
        });

        if let Some(entry) = entry {
            if now_ms() - entry.fetched_at >= self.ttl.as_millis() as i64 {
                drop(self.refresh(nip19, self.limits.fetch_timeout));
            }
            return Ok(Some(entry.jpeg.into()));
        }

        self.refresh(nip19, self.query_timeout).await
    }

    /// Render and store the image, at most once at a time per identifier.
    fn refresh(
        self: &Arc<Self>,
        nip19: &str,
        relay_timeout: Duration,
    ) -> impl Future<Output = RenderResult> + use<> {
        let this = self.clone();
        let key = nip19.to_string();
        self.in_flight.run(key.clone(), move || async move {
            let jpeg = this
                .resolve(&key, Instant::now() + relay_timeout)
                .await
                .map_err(Arc::new)?;
            if let Some(jpeg) = &jpeg {
                let entry = OgImageEntry {
                    jpeg: jpeg.to_vec(),
                    fetched_at: now_ms(),
                };
                if let Err(e) = this.db.put_og_image(&key, &entry) {
                    tracing::error!("OG image cache write failed: {e:#}");
                }
            }
            Ok(jpeg)
        })
    }

    async fn resolve(&self, identifier: &str, deadline: Instant) -> anyhow::Result<Option<Bytes>> {
        let Ok(decoded) = Nip19::from_bech32(identifier) else {
            return Ok(None);
        };

        let filter = match decoded {
            Nip19::Pubkey(pk) => return self.render_profile(pk.to_hex(), deadline).await.map(Some),
            Nip19::Profile(p) => {
                return self
                    .render_profile(p.public_key.to_hex(), deadline)
                    .await
                    .map(Some);
            }
            Nip19::EventId(id) => json!({ "ids": [id.to_hex()], "limit": 1 }),
            Nip19::Event(e) => json!({ "ids": [e.event_id.to_hex()], "limit": 1 }),
            Nip19::Coordinate(c) => json!({
                "kinds": [u16::from(c.kind)],
                "authors": [c.public_key.to_hex()],
                "#d": [c.identifier],
                "limit": 1,
            }),
            _ => return Ok(None),
        };

        let events = self.relays.query(vec![filter], deadline).await;
        let Some(job) = events.first().and_then(event_job) else {
            return Ok(None);
        };
        self.render(job).await.map(Some)
    }

    async fn render_profile(&self, pubkey: String, deadline: Instant) -> anyhow::Result<Bytes> {
        let filter = json!({ "kinds": [0, 16767], "authors": [pubkey] });
        let events = self.relays.query(vec![filter], deadline).await;
        self.render(profile_job(&events)).await
    }

    /// Draw a job. The CPU-bound steps run on blocking threads, at most one
    /// per spare core at a time; fetching images in between holds no slot.
    async fn render(&self, job: Job) -> anyhow::Result<Bytes> {
        let (svg, mut images) = self.blocking(move || job.build()).await?;
        render::fetch_images(&self.client, &svg, &self.limits, &mut images).await;
        let jpeg = self
            .blocking(move || render::svg_to_jpeg(&svg, images))
            .await??;
        Ok(jpeg.into())
    }

    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> anyhow::Result<T> {
        let _slot = self.renders.acquire().await.context("render pool closed")?;
        tokio::task::spawn_blocking(f)
            .await
            .context("render panicked")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: u64, tags: &[&[&str]], content: &str) -> Event {
        Event {
            id: format!("{kind}"),
            pubkey: "pk".into(),
            created_at: 0,
            kind,
            tags: tags
                .iter()
                .map(|t| t.iter().map(|s| s.to_string()).collect())
                .collect(),
            content: content.into(),
        }
    }

    #[test]
    fn picks_renderers_by_kind() {
        let colors: &[&[&str]] = &[
            &["c", "#000000", "background"],
            &["c", "#ffffff", "text"],
            &["c", "#ff0000", "primary"],
        ];
        assert!(matches!(
            event_job(&event(36767, colors, "")),
            Some(Job::Theme(_))
        ));
        assert!(matches!(
            event_job(&event(16767, colors, "")),
            Some(Job::Theme(_))
        ));
        assert!(event_job(&event(36767, &[], "")).is_none());
        assert!(matches!(
            event_job(&event(3367, &[&["c", "#123456"]], "")),
            Some(Job::Palette(_))
        ));
        assert!(event_job(&event(3367, &[], "")).is_none());
        assert!(event_job(&event(1, &[], "hello")).is_none());
    }

    #[test]
    fn builds_profiles_from_kind_0_and_the_active_theme() {
        let profile = event(
            0,
            &[],
            r#"{"picture":"https://a.example/me.png","shape":"⭐"}"#,
        );
        let Job::Profile(data) = profile_job(std::slice::from_ref(&profile)) else {
            panic!()
        };
        assert_eq!(data.avatar_url.as_deref(), Some("https://a.example/me.png"));
        assert_eq!(data.shape.as_deref(), Some("⭐"));
        assert_eq!(data.theme, Theme::default_profile());

        let theme = event(
            16767,
            &[
                &["c", "#ffffff", "background"],
                &["c", "#000000", "text"],
                &["c", "#ff0000", "primary"],
            ],
            "",
        );
        let Job::Profile(data) = profile_job(&[theme, profile]) else {
            panic!()
        };
        assert_eq!(data.theme.colors.background, "0 0% 100%");

        let Job::Profile(data) = profile_job(&[event(0, &[], r#"{"picture":""}"#)]) else {
            panic!()
        };
        assert_eq!(data.avatar_url, None);
    }

    #[test]
    fn builds_masked_profiles_and_emoji_palettes() {
        let Job::Profile(data) = profile_job(&[event(0, &[], r#"{"shape":"⭐"}"#)]) else {
            panic!()
        };
        let (svg, images) = Job::Profile(data).build();
        assert!(svg.contains(MASK_HREF));
        assert!(images.contains_key(MASK_HREF));

        let p = palette::parse_palette_event(&event(3367, &[&["c", "#123456"]], "🎨")).unwrap();
        let (svg, images) = Job::Palette(p).build();
        assert!(svg.contains(EMOJI_HREF));
        let jpeg = render::svg_to_jpeg(&svg, images).unwrap();
        assert_eq!(&jpeg[..2], [0xff, 0xd8]);
    }
}
