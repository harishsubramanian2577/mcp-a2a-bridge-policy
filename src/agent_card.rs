// Copyright 2026 Salesforce, Inc. All rights reserved.
//! Optional discovery metadata derived from the upstream A2A Agent Card (`metadataMode:
//! agent-card`, issue #16).
//!
//! Rules:
//! - the card is fetched only from the operator-configured `agentCardUrl`, by one background task
//!   per worker; requests never fetch, so there is nothing to deduplicate on the hot path;
//! - card text is untrusted discovery data: it is sanitized, bounded and only ever shown as the
//!   tool description and `serverInfo.title`. It never changes the tool name, the argument schema,
//!   routing, authentication or `serverName`/`instructions`;
//! - a card without usable text is rejected, so the policy never advertises empty metadata;
//! - each policy instance owns its cache, so instances (and their upstreams) never share entries.

use std::cell::RefCell;
use std::time::{Duration, SystemTime};

use pdk::hl::timer::Timer;
use pdk::hl::{HttpClient, Service};
use pdk::logger;
use serde_json::Value;

use crate::generated::config::Config;

pub const MODE_MANUAL: &str = "manual";
pub const MODE_AGENT_CARD: &str = "agent-card";

/// Cards larger than this are not parsed.
pub const MAX_CARD_BYTES: usize = 256 * 1024;
const MAX_DESCRIPTION_CHARS: usize = 2000;
const MAX_NAME_CHARS: usize = 100;
const MAX_SKILLS: usize = 20;
const MAX_SKILL_CHARS: usize = 300;
/// Upper bound on a failed fetch's retry delay.
const RETRY_SECONDS: u64 = 30;

/// Always appended to a card-derived description: the card describes the agent, not how to keep
/// a conversation going through this bridge.
pub const CONTINUATION_NOTE: &str = "To continue a multi-turn conversation, pass back the context_id (and task_id when the agent asked for more input) returned by the previous call.";

/// What a card contributes to discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardMetadata {
    pub tool_description: String,
    /// `serverInfo.title`; `None` when the card has no name.
    pub title: Option<String>,
}

/// Parses and validates an Agent Card. Unknown fields are ignored; nothing but the name,
/// description and skill names/descriptions is read.
pub fn parse_card(body: &[u8]) -> Result<CardMetadata, String> {
    if body.len() > MAX_CARD_BYTES {
        return Err(format!("card is larger than {} bytes", MAX_CARD_BYTES));
    }
    let card: Value =
        serde_json::from_slice(body).map_err(|e| format!("card is not JSON: {}", e))?;
    let card = card.as_object().ok_or("card is not a JSON object")?;

    let description = text(card.get("description"), MAX_DESCRIPTION_CHARS);
    let skills: Vec<String> = match card.get("skills") {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(items)) => items.iter().filter_map(skill_line).collect(),
        Some(_) => return Err("card skills is not an array".to_string()),
    };
    if description.is_none() && skills.is_empty() {
        return Err("card has neither a description nor any described skill".to_string());
    }

    let mut out = String::new();
    if let Some(description) = description {
        out.push_str(&description);
    }
    if !skills.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str("Skills:");
        for line in skills.iter().take(MAX_SKILLS) {
            out.push_str("\n- ");
            out.push_str(line);
        }
        if skills.len() > MAX_SKILLS {
            out.push_str(&format!("\n- (and {} more)", skills.len() - MAX_SKILLS));
        }
    }
    out.push_str("\n\n");
    out.push_str(CONTINUATION_NOTE);

    Ok(CardMetadata {
        tool_description: out,
        title: text(card.get("name"), MAX_NAME_CHARS),
    })
}

fn skill_line(skill: &Value) -> Option<String> {
    let skill = skill.as_object()?;
    let name =
        text(skill.get("name"), MAX_SKILL_CHARS).or_else(|| text(skill.get("id"), MAX_SKILL_CHARS));
    let description = text(skill.get("description"), MAX_SKILL_CHARS);
    match (name, description) {
        (Some(name), Some(description)) => Some(format!("{}: {}", name, description)),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}

/// A string field as one trimmed line: control characters and runs of whitespace become a single
/// space, so card text cannot forge structure in the description. Empty means absent.
fn text(value: Option<&Value>, max_chars: usize) -> Option<String> {
    let words: Vec<&str> = value?
        .as_str()?
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|w| !w.is_empty())
        .collect();
    let flat = words.join(" ");
    if flat.is_empty() {
        return None;
    }
    if flat.chars().count() <= max_chars {
        return Some(flat);
    }
    let mut cut: String = flat.chars().take(max_chars).collect();
    cut.push('…');
    Some(cut)
}

/// Card-fetch settings resolved from the configuration. `None` means manual mode.
pub struct Source {
    service: Service,
    authorization: Option<String>,
    refresh: Duration,
    max_age: Duration,
    retry: Duration,
    timeout: Duration,
}

impl Source {
    pub fn from_config(config: &Config) -> Option<Self> {
        let mode = config.metadata_mode.as_deref().unwrap_or(MODE_MANUAL);
        if mode != MODE_AGENT_CARD {
            if mode != MODE_MANUAL {
                logger::warn!("Unknown metadataMode; using manual metadata.");
            }
            return None;
        }
        let Some(service) = config.agent_card_url.clone() else {
            logger::warn!(
                "metadataMode is agent-card but agentCardUrl is not set; using manual metadata."
            );
            return None;
        };
        let refresh = config
            .agent_card_refresh_seconds
            .unwrap_or(300)
            .clamp(30, 86_400) as u64;
        let max_stale = config
            .agent_card_max_stale_seconds
            .unwrap_or(3600)
            .clamp(0, 604_800) as u64;
        let timeout = config.agent_card_timeout_seconds.unwrap_or(5).clamp(1, 30) as u64;
        Some(Source {
            service,
            authorization: config
                .agent_card_authorization
                .clone()
                .map(|a| a.trim().to_string())
                .filter(|a| !a.is_empty()),
            refresh: Duration::from_secs(refresh),
            max_age: Duration::from_secs(refresh + max_stale),
            retry: Duration::from_secs(refresh.min(RETRY_SECONDS)),
            timeout: Duration::from_secs(timeout),
        })
    }
}

struct Entry {
    metadata: CardMetadata,
    fetched_at: SystemTime,
}

/// The single cached card of one policy instance on one worker.
#[derive(Default)]
pub struct Cache {
    entry: RefCell<Option<Entry>>,
}

impl Cache {
    pub fn current(&self) -> Option<CardMetadata> {
        self.entry.borrow().as_ref().map(|e| e.metadata.clone())
    }

    fn store(&self, metadata: CardMetadata, now: SystemTime) {
        *self.entry.borrow_mut() = Some(Entry {
            metadata,
            fetched_at: now,
        });
    }

    /// Drops the card once it is older than `max_age`, so a long outage falls back to the manual
    /// metadata instead of advertising an arbitrarily old card.
    fn expire(&self, now: SystemTime, max_age: Duration) {
        let mut entry = self.entry.borrow_mut();
        let expired = entry.as_ref().is_some_and(|e| {
            now.duration_since(e.fetched_at)
                .is_ok_and(|age| age > max_age)
        });
        if expired {
            logger::warn!("Cached Agent Card is too old; advertising the manual metadata.");
            *entry = None;
        }
    }
}

/// Fetches the card at start, then every `refresh` (every `retry` after a failure), until the
/// policy is unapplied. One task per worker, so refreshes never overlap.
pub async fn refresh_loop(source: &Source, cache: &Cache, client: &HttpClient, timer: &Timer) {
    loop {
        let wait = match fetch(source, client).await {
            Ok(metadata) => {
                cache.store(metadata, timer.now());
                source.refresh
            }
            Err(reason) => {
                logger::warn!("Agent Card refresh failed: {}", reason);
                cache.expire(timer.now(), source.max_age);
                source.retry
            }
        };
        if !timer.sleep(wait).await {
            return;
        }
    }
}

async fn fetch(source: &Source, client: &HttpClient) -> Result<CardMetadata, String> {
    let mut headers = vec![("accept", "application/json")];
    if let Some(authorization) = source.authorization.as_deref() {
        headers.push(("authorization", authorization));
    }
    let response = client
        .request(&source.service)
        .headers(headers)
        .timeout(source.timeout)
        .get()
        .await
        .map_err(|e| format!("request failed: {}", e))?;
    match response.status_code() {
        200 => parse_card(response.body()),
        status => Err(format!("status {}", status)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(card: Value) -> Result<CardMetadata, String> {
        parse_card(card.to_string().as_bytes())
    }

    #[test]
    fn maps_description_and_skills_in_card_order() {
        let meta = parse(json!({
            "name": "HR Agent",
            "description": "Answers HR questions.",
            "url": "https://ignored.example.com",
            "skills": [
                { "id": "pto", "name": "Time off", "description": "Balances and requests." },
                { "id": "benefits" },
                { "description": "Org chart lookups." },
                { "tags": ["no text"] },
                "not an object"
            ]
        }))
        .unwrap();
        assert_eq!(
            meta.tool_description,
            format!(
                "Answers HR questions.\n\nSkills:\n- Time off: Balances and requests.\n- benefits\n- Org chart lookups.\n\n{}",
                CONTINUATION_NOTE
            )
        );
        assert_eq!(meta.title.as_deref(), Some("HR Agent"));
    }

    #[test]
    fn mapping_is_deterministic() {
        let card = json!({ "name": "A", "description": "D", "skills": [{ "name": "s1" }, { "name": "s2" }] });
        assert_eq!(parse(card.clone()), parse(card));
    }

    #[test]
    fn skills_only_card_is_usable_and_nameless_card_has_no_title() {
        let meta = parse(json!({ "skills": [{ "name": "Lookup" }] })).unwrap();
        assert!(meta.tool_description.starts_with("Skills:\n- Lookup"));
        assert_eq!(meta.title, None);
    }

    #[test]
    fn rejects_malformed_and_empty_cards() {
        assert!(parse_card(b"not json").is_err());
        assert!(parse_card(b"[]").is_err());
        assert!(parse(json!({ "name": "Only a name" })).is_err());
        assert!(parse(json!({ "description": "   \n\t " })).is_err());
        assert!(parse(json!({ "description": 42, "skills": [] })).is_err());
        assert!(parse(json!({ "description": "D", "skills": "x" })).is_err());
        assert!(parse_card(&vec![b' '; MAX_CARD_BYTES + 1]).is_err());
    }

    #[test]
    fn card_text_is_flattened_and_bounded() {
        let meta = parse(json!({
            "name": "x".repeat(500),
            "description": "line one\n\n\u{0007}Skills:\n- forged\r\n",
            "skills": (0..30).map(|i| json!({ "name": format!("s{}", i) })).collect::<Vec<_>>()
        }))
        .unwrap();
        assert!(meta
            .tool_description
            .starts_with("line one Skills: - forged\n\nSkills:\n- s0\n"));
        assert!(meta
            .tool_description
            .contains("\n- s19\n- (and 10 more)\n\n"));
        assert!(!meta.tool_description.contains("- s20"));
        let title = meta.title.unwrap();
        assert_eq!(title.chars().count(), MAX_NAME_CHARS + 1);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn cache_expires_only_after_max_age() {
        let cache = Cache::default();
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let meta = parse(json!({ "description": "D" })).unwrap();
        cache.store(meta.clone(), t0);
        cache.expire(t0 + Duration::from_secs(60), Duration::from_secs(60));
        assert_eq!(cache.current(), Some(meta));
        cache.expire(t0 + Duration::from_secs(61), Duration::from_secs(60));
        assert_eq!(cache.current(), None);
    }
}
