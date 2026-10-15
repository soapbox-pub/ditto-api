# Ditto API

The API behind [api.ditto.pub](https://api.ditto.pub): link previews, favicons,
and generated Open Graph images for [Ditto](https://ditto.pub).

A Rust rewrite of the API half of
[ditto-server](https://gitlab.com/soapbox-pub/ditto-server), and a drop-in
replacement for it on an API-only host. It serves no frontend, does no crawler
meta injection, and has no sitemap; those stay with whatever serves ditto.pub.

## Usage

```sh
cp .env.example .env
cargo run --release
```

It ships as one binary, with the emoji font compiled in. The cache is a
single SQLite file at `DB_PATH`.

```sh
# Drop a stored link preview so the next request refetches it. Accepts the
# previewed URL or the link-preview/ URL that wraps it.
revoke-link-preview <url>...
```

## Endpoints

Every route is served both bare (`/favicon/…`) and under `/api`
(`/api/favicon/…`), so a reverse proxy that still maps bare paths onto
ditto-server's `/api/*` namespace works unchanged. Anything else is a 404.

### `GET /link-preview/{url}`

An OEmbed-compatible JSON preview of an https page. `{url}` is the target,
percent-encoded; the request's own query string is appended to it. Uses the
page's native OEmbed endpoint if it advertises one, otherwise synthesizes one
from its meta tags. Wikidata entity URLs are special-cased. Image URLs in the
result point at `/link-preview-image/`.

Results are cached for `LINK_PREVIEW_CACHE_TTL`, failures for
`LINK_PREVIEW_ERROR_TTL`. Stale entries are served while they refresh in
the background, concurrent requests for one URL share a single fetch, and
past `LINK_PREVIEW_QUEUE_LIMIT` queued fetches requests get a 503.

### `GET /link-preview-image/{url}`

Proxies an https image with the link preview User-Agent. Some sites, such as
Reddit, show crawlers a different image than browsers. Non-images get a 415.

### `GET /favicon/{hostname}`

The first `<link rel="…icon…">` on `https://{hostname}`, falling back to
`/favicon.ico`.

### `GET /og-image/{nip19}`

A 1200×630 JPEG for a Nostr entity, cached for `OG_IMAGE_CACHE_TTL`:

- `npub` / `nprofile`: the avatar on the profile's active theme (kind 16767),
  masked to the profile's emoji `shape` if it has one
- `naddr` / `nevent` / `note` for themes (kinds 16767, 36767): a theme mockup
- the same for Color Moments (kind 3367): the palette, with its emoji

Other kinds and unknown entities are a 404.

## Configuration

See [`.env.example`](.env.example). It follows ditto-server's variables, minus
the frontend-only ones. `DB_DIR` (LMDB) is replaced by `DB_PATH` (SQLite).
Existing ditto-server caches aren't migrated, so caches start cold.

## Deployment

api.ditto.pub runs on CT 126 (`ditto-api.lan`, Ubuntu 26.04) on bacchus.
Caddy on venus terminates TLS and proxies to it over WireGuard (`10.10.2.4`,
peer of venus's `wg0`). The binary lives in `/opt/ditto-api/bin`, its config
in `/opt/ditto-api/.env`, its cache in `/opt/ditto-api/data`, and it runs
under [`deploy/ditto-api.service`](deploy/ditto-api.service).

```sh
cargo build --release
scp target/release/{ditto-api,revoke-link-preview} root@ditto-api.lan:/opt/ditto-api/bin/
ssh root@ditto-api.lan systemctl restart ditto-api
```

Ubuntu 26.04's AppArmor profile for `wg-quick` kills it inside an
unprivileged container (it may not map its own interpreter), so it is
disabled there: `ln -s /etc/apparmor.d/wg-quick /etc/apparmor.d/disable/`.

## Development

```sh
cargo test
cargo clippy --all-targets
cargo fmt
```
