# Ditto API

The API behind [api.ditto.pub](https://api.ditto.pub): link previews and
favicons for [Ditto](https://ditto.pub).

A Rust rewrite of the API half of
[ditto-server](https://gitlab.com/soapbox-pub/ditto-server), and a drop-in
replacement for it on an API-only host. It serves no frontend, does no crawler
meta injection, has no sitemap, and doesn't generate OG images
(`/og-image/`, which nothing used).

## Usage

```sh
cp .env.example .env
cargo run --release
```

It ships as one binary. The cache is a single SQLite file at `DB_PATH`.

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

## Configuration

See [`.env.example`](.env.example). It follows ditto-server's variables, minus
the frontend-only ones and those for OG images, which ditto-api doesn't
generate. `DB_DIR` (LMDB) is replaced by `DB_PATH` (SQLite).
Existing ditto-server caches aren't migrated, so caches start cold.

## Deployment

Build with `cargo build --release` and install `ditto-api` and
`revoke-link-preview` wherever you keep binaries.
[`deploy/ditto-api.service`](deploy/ditto-api.service) is a sample systemd
unit, which expects the binaries in `/opt/ditto-api/bin`, the config in
`/opt/ditto-api/.env`, and `DB_PATH` under `/opt/ditto-api/data`.

Put it behind whatever terminates TLS. ditto-api sets every header its
responses need itself, Content-Security-Policy included, so the proxy
doesn't have to add any.

## Development

```sh
cargo test
cargo clippy --all-targets
cargo fmt
```
