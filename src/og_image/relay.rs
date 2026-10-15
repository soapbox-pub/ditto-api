//! A minimal Nostr relay client: one-shot queries across a set of relays.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

/// A signed Nostr event.
#[derive(Debug, Clone, Deserialize)]
pub struct Event {
    pub id: String,
    pub pubkey: String,
    pub created_at: u64,
    pub kind: u64,
    pub tags: Vec<Vec<String>>,
    pub content: String,
}

impl Event {
    /// The tag named `name`.
    pub fn tag(&self, name: &str) -> Option<&Vec<String>> {
        self.tags
            .iter()
            .find(|t| t.first().is_some_and(|n| n == name))
    }

    /// The first value of the tag named `name`: `tags.find(...)?.[1]`.
    pub fn tag_value(&self, name: &str) -> Option<&str> {
        self.tag(name)?.get(1).map(String::as_str)
    }

    fn replace_key(&self) -> Option<String> {
        let d = || self.tag_value("d").unwrap_or_default();
        match self.kind {
            0 | 3 | 10_000..20_000 => Some(format!("{}:{}", self.kind, self.pubkey)),
            30_000..40_000 => Some(format!("{}:{}:{}", self.kind, self.pubkey, d())),
            _ => None,
        }
    }
}

/// The relays queried for OG image data.
pub struct Relays {
    urls: Vec<String>,
}

static SUB_ID: AtomicU64 = AtomicU64::new(0);

impl Relays {
    pub fn new(urls: Vec<String>) -> Self {
        Self { urls }
    }

    /// Query every relay with `filters`, giving up at `deadline`.
    ///
    /// Each relay contributes whatever valid events it sent before its EOSE,
    /// the deadline, or an error, whichever came first. Duplicates are
    /// dropped, only the newest version of a replaceable event is kept, and
    /// the result is newest first.
    pub async fn query(&self, filters: Vec<Value>, deadline: Instant) -> Vec<Event> {
        let results = futures::future::join_all(
            self.urls
                .iter()
                .map(|url| query_relay(url, &filters, deadline)),
        )
        .await;

        let mut by_id = HashMap::new();
        for event in results.into_iter().flatten() {
            by_id.entry(event.id.clone()).or_insert(event);
        }

        let mut newest: HashMap<String, Event> = HashMap::new();
        let mut events = Vec::new();
        for event in by_id.into_values() {
            match event.replace_key() {
                Some(key) => {
                    let keep = newest.get(&key).is_none_or(|e| newer(&event, e));
                    if keep {
                        newest.insert(key, event);
                    }
                }
                None => events.push(event),
            }
        }
        events.extend(newest.into_values());
        events.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        events
    }
}

/// NIP-01's tiebreak: later `created_at`, then the lower id.
fn newer(a: &Event, b: &Event) -> bool {
    (a.created_at, std::cmp::Reverse(&a.id)) > (b.created_at, std::cmp::Reverse(&b.id))
}

async fn query_relay(url: &str, filters: &[Value], deadline: Instant) -> Vec<Event> {
    let mut events = Vec::new();
    let collect = collect_events(url, filters, &mut events);
    match tokio::time::timeout_at(deadline, collect).await {
        Ok(Err(e)) => tracing::debug!("relay {url}: {e:#}"),
        Err(_) => tracing::debug!("relay {url}: timed out"),
        Ok(Ok(())) => {}
    }
    events
}

async fn collect_events(
    url: &str,
    filters: &[Value],
    events: &mut Vec<Event>,
) -> anyhow::Result<()> {
    let (mut ws, _) = tokio_tungstenite::connect_async(url).await?;
    let sub_id = format!("ditto-api-{}", SUB_ID.fetch_add(1, Ordering::Relaxed));

    let mut req = vec![json!("REQ"), json!(sub_id)];
    req.extend(filters.iter().cloned());
    ws.send(Message::text(Value::Array(req).to_string()))
        .await?;

    while let Some(msg) = ws.next().await {
        let msg = msg?;
        let Message::Text(text) = msg else { continue };
        let Ok(Value::Array(parts)) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if parts.get(1).and_then(Value::as_str) != Some(sub_id.as_str()) {
            continue;
        }
        match parts.first().and_then(Value::as_str) {
            Some("EVENT") => {
                if let Some(event) = parts.get(2).and_then(verified) {
                    events.push(event);
                }
            }
            Some("EOSE") | Some("CLOSED") => break,
            _ => {}
        }
    }

    let close = json!(["CLOSE", sub_id]).to_string();
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        ws.send(Message::text(close)).await?;
        ws.close(None).await
    })
    .await;
    Ok(())
}

/// Parse an event, keeping it only if its id and signature check out.
fn verified(raw: &Value) -> Option<Event> {
    let checked: nostr::prelude::Event = serde_json::from_value(raw.clone()).ok()?;
    checked.verify().ok()?;
    serde_json::from_value(raw.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_forged_events() {
        let mut raw = json!({
            "kind": 1,
            "id": "33c758466a465ce9df004b6d6e3abb039d593277784c276c49725a622794eca0",
            "pubkey": "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
            "created_at": 1700000000,
            "tags": [],
            "content": "hi",
            "sig": "b0d0c33dd75e00e3bc023391e24c3a8c3e75271196a8f7a77f99b04a8b57b04eb3b24983f0400365573f651ced92c232085c5777c82fed6a198eaf911c05de9e",
        });
        assert!(verified(&raw).is_some());
        raw["content"] = json!("bye");
        assert!(verified(&raw).is_none());
    }

    #[test]
    fn prefers_newer_then_lower_id() {
        let ev = |id: &str, t| Event {
            id: id.into(),
            pubkey: "p".into(),
            created_at: t,
            kind: 0,
            tags: vec![],
            content: String::new(),
        };
        assert!(newer(&ev("b", 2), &ev("a", 1)));
        assert!(newer(&ev("a", 1), &ev("b", 1)));
        assert!(!newer(&ev("b", 1), &ev("a", 1)));
    }
}
