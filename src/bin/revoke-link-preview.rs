//! Drop stored link previews so the next request refetches them.
//!
//! Accepts the previewed URL or the `link-preview/` URL that wraps it, since
//! the latter is what shows up in logs and browser network panels.

use ditto_api::config::db_path;
use ditto_api::db::Db;
use ditto_api::link_preview::cache::revoke;
use percent_encoding::percent_decode_str;

/// The URL the cache is keyed on, unwrapped the same way the route does it:
/// everything after the route prefix is percent-decoded, and the wrapping
/// URL's own query string is appended.
fn target_url_for(arg: &str) -> String {
    let marker = "/link-preview/";
    let Some(at) = arg.find(marker) else {
        return arg.to_string();
    };
    let rest = &arg[at + marker.len()..];
    let (path, query) = match rest.find('?') {
        Some(q) => rest.split_at(q),
        None => (rest, ""),
    };
    let decoded = percent_decode_str(path).decode_utf8_lossy();
    format!("{decoded}{}", if query == "?" { "" } else { query })
}

fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("Usage: revoke-link-preview <url>...");
        eprintln!();
        eprintln!("Deletes stored link previews so the next request refetches them.");
        eprintln!("Reads DB_PATH from the environment, same as the server.");
        std::process::exit(1);
    }

    let db = Db::open(&db_path(&|k| std::env::var(k).ok()))?;
    for arg in args {
        let target = target_url_for(&arg);
        let removed = revoke(&db, &target)?;
        println!(
            "{}  {target}",
            if removed { "revoked   " } else { "not cached" }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwraps_endpoint_urls() {
        assert_eq!(target_url_for("https://a.example/x"), "https://a.example/x");
        assert_eq!(
            target_url_for("https://api.ditto.pub/link-preview/https%3A%2F%2Fa.example%2Fx?q=1"),
            "https://a.example/x?q=1"
        );
        assert_eq!(
            target_url_for("https://ditto.pub/api/link-preview/https%3A%2F%2Fa.example%2Fx"),
            "https://a.example/x"
        );
    }
}
