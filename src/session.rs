// Copyright 2026 Salesforce, Inc. All rights reserved.
//! Optional server-side memory of the A2A conversation behind an MCP session. It is a fallback
//! for clients that do not echo `context_id`; the tool argument always wins.
//!
//! Isolation rules:
//! - session ids are random 128-bit UUIDs, never derived from caller-controlled metadata;
//! - a session id is only honoured after `initialize` registered it, so a caller cannot plant one;
//! - storage keys are a SHA-256 over length-prefixed (scope, session id, identity) fields, so no
//!   two distinct inputs share a key;
//! - the namespace is scoped to the API instance and policy, so instances sharing Redis are
//!   isolated while replicas of one instance still share sessions.

use std::cell::Cell;
use std::time::{SystemTime, UNIX_EPOCH};

use pdk::data_storage::{
    DataStorage, DataStorageBuilder, LocalDataStorage, RemoteDataStorage, StoreMode,
};
use pdk::logger;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const NAMESPACE_PREFIX: &str = "mcp-a2a-sessions";
const KEY_FORMAT: &[u8] = b"mcp-a2a-session-key/v1";
/// Local storage has no TTL, so expired entries are swept every this many writes.
const LOCAL_SWEEP_INTERVAL: u32 = 256;
/// MCP session ids are visible ASCII; anything else (or anything longer) was not issued here.
const MAX_SESSION_ID_LEN: usize = 128;
pub const MIN_TTL_SECONDS: u64 = 60;
pub const MAX_TTL_SECONDS: u64 = 86_400;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEntry {
    /// `None` until the agent returned the first context of this session.
    pub context_id: Option<String>,
    /// Only set while the agent is waiting on the caller (input-required / auth-required).
    pub task_id: Option<String>,
    /// Last write, in Unix seconds. Expiry is judged against the TTL configured now, so lowering
    /// `sessionTtlSeconds` also shortens the life of entries written before the change.
    pub touched_at: u64,
}

impl SessionEntry {
    pub fn is_live(&self, now: u64, ttl_seconds: u64) -> bool {
        now < self.touched_at.saturating_add(ttl_seconds)
    }
}

enum Backend {
    Local(LocalDataStorage),
    Remote(RemoteDataStorage),
}

pub struct SessionStore {
    backend: Backend,
    ttl_seconds: u64,
    writes: Cell<u32>,
}

impl SessionStore {
    /// `None` for `disabled` (or any unknown mode). `remote` panics inside PDK at startup when
    /// the gateway has no Shared Storage configured, which surfaces the misconfiguration early.
    pub fn from_config(
        builder: &DataStorageBuilder,
        mode: &str,
        ttl_seconds: u64,
        namespace: &str,
    ) -> Option<Self> {
        let ttl_seconds = clamp_ttl(ttl_seconds);
        let backend = match mode {
            "local" => Backend::Local(builder.local(namespace)),
            "remote" => Backend::Remote(builder.remote(namespace, remote_ttl_millis(ttl_seconds))),
            _ => return None,
        };
        Some(SessionStore {
            backend,
            ttl_seconds,
            writes: Cell::new(0),
        })
    }

    async fn get(&self, key: &str) -> Option<(SessionEntry, String)> {
        let found = match &self.backend {
            Backend::Local(s) => s.get::<SessionEntry>(key).await,
            Backend::Remote(s) => s.get::<SessionEntry>(key).await,
        };
        match found {
            Ok(Some((entry, cas))) if entry.is_live(now_seconds(), self.ttl_seconds) => {
                Some((entry, cas))
            }
            Ok(_) => None,
            Err(err) => {
                // Fail open: losing the remembered context only starts a new conversation.
                logger::warn!("Session store read failed for {}: {}", key_hint(key), err);
                None
            }
        }
    }

    async fn put(&self, key: &str, mode: &StoreMode, entry: &SessionEntry) -> bool {
        let stored = match &self.backend {
            Backend::Local(s) => s.store(key, mode, entry).await,
            Backend::Remote(s) => s.store(key, mode, entry).await,
        };
        self.sweep_if_due().await;
        match stored {
            Ok(()) => true,
            Err(err) => {
                logger::warn!("Session store write failed for {}: {}", key_hint(key), err);
                false
            }
        }
    }

    fn fresh_entry(&self, context_id: Option<String>, task_id: Option<String>) -> SessionEntry {
        SessionEntry {
            context_id,
            task_id,
            touched_at: now_seconds(),
        }
    }

    /// Records a newly issued session id. Only registered sessions are ever read or updated.
    pub async fn register(&self, key: &str) -> bool {
        self.put(key, &StoreMode::Absent, &self.fresh_entry(None, None))
            .await
    }

    /// The live entry of a registered session; `None` for unknown, expired or deleted sessions.
    pub async fn load(&self, key: &str) -> Option<SessionEntry> {
        self.get(key).await.map(|(entry, _)| entry)
    }

    /// Remembers the latest context. A session deleted (or expired) while the call was in flight
    /// stays deleted: the compare-and-swap only updates the entry that is still there.
    pub async fn update(&self, key: &str, context_id: String, task_id: Option<String>) {
        let Some((_, cas)) = self.get(key).await else {
            logger::debug!(
                "Session {} ended before the agent answered; not remembered.",
                key_hint(key)
            );
            return;
        };
        let entry = self.fresh_entry(Some(context_id), task_id);
        self.put(key, &StoreMode::Cas(cas), &entry).await;
    }

    pub async fn remove(&self, key: &str) {
        let removed = match &self.backend {
            Backend::Local(s) => s.delete(key).await,
            Backend::Remote(s) => s.delete(key).await,
        };
        if let Err(err) = removed {
            logger::warn!("Session store delete failed for {}: {}", key_hint(key), err);
        }
    }

    async fn sweep_if_due(&self) {
        let Backend::Local(storage) = &self.backend else {
            return;
        };
        let writes = self.writes.get().wrapping_add(1);
        self.writes.set(writes);
        if !writes.is_multiple_of(LOCAL_SWEEP_INTERVAL) {
            return;
        }
        let Ok(keys) = storage.get_keys().await else {
            return;
        };
        let now = now_seconds();
        for key in keys {
            if let Ok(Some((entry, _))) = storage.get::<SessionEntry>(&key).await {
                if !entry.is_live(now, self.ttl_seconds) {
                    let _ = storage.delete(&key).await;
                }
            }
        }
    }
}

pub fn clamp_ttl(ttl_seconds: u64) -> u64 {
    ttl_seconds.clamp(MIN_TTL_SECONDS, MAX_TTL_SECONDS)
}

/// The Redis TTL matches the configured TTL that `is_live` checks, so both expire together.
pub fn remote_ttl_millis(ttl_seconds: u64) -> u32 {
    u32::try_from(clamp_ttl(ttl_seconds) * 1000).unwrap_or(u32::MAX)
}

/// One namespace per API instance and policy. Replicas of the same instance share it; other
/// instances on the same Redis do not. The base path stands in when the API id is unknown
/// (local mode).
pub fn namespace(api_id: Option<&str>, base_path: Option<&str>, policy_name: &str) -> String {
    let api = api_id
        .filter(|id| !id.is_empty())
        .or(base_path)
        .unwrap_or("-");
    format!("{}/{}/{}", NAMESPACE_PREFIX, api, policy_name)
}

/// SHA-256 over length-prefixed fields. `identity` is `None` only when no identity binding is
/// configured; a configured but missing identity never reaches this function.
pub fn session_key(scope: &str, session_id: &str, identity: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(KEY_FORMAT);
    for field in [Some(scope), Some(session_id), identity] {
        match field {
            Some(value) => {
                hasher.update([1u8]);
                hasher.update((value.len() as u64).to_be_bytes());
                hasher.update(value.as_bytes());
            }
            None => hasher.update([0u8]),
        }
    }
    hex(&hasher.finalize())
}

pub fn is_valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= MAX_SESSION_ID_LEN
        && session_id.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

/// A random (version 4) UUID from the host's secure random source; `None` if it is unavailable,
/// in which case no session is issued rather than a guessable one.
pub fn random_uuid() -> Option<String> {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).ok()?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex(&b);
    Some(format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    ))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Keys are hashes, but logs still only carry a prefix.
fn key_hint(key: &str) -> String {
    format!("session#{}", &key[..key.len().min(12)])
}

pub fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn session_key_binds_scope_session_and_identity() {
        let a = session_key("scope", "s1", Some("client-a"));
        assert_eq!(a, session_key("scope", "s1", Some("client-a")));
        assert_eq!(a.len(), 64);
        assert_ne!(a, session_key("scope", "s1", Some("client-b")));
        assert_ne!(a, session_key("scope", "s1", None));
        assert_ne!(a, session_key("other-scope", "s1", Some("client-a")));
        assert!(!a.contains("s1"), "the key must not reveal the session id");
    }

    #[test]
    fn session_key_encoding_is_unambiguous() {
        // With "<sid>:<identity>" concatenation these pairs collided.
        assert_ne!(
            session_key("x", "a:b", Some("c")),
            session_key("x", "a", Some("b:c"))
        );
        assert_ne!(
            session_key("x", "ab", None),
            session_key("x", "a", Some("b"))
        );
        assert_ne!(session_key("x", "s", Some("")), session_key("x", "s", None));
        assert_ne!(session_key("xs", "1", None), session_key("x", "s1", None));
    }

    #[test]
    fn random_uuids_are_unique_v4() {
        let ids: HashSet<String> = (0..1000).map(|_| random_uuid().unwrap()).collect();
        assert_eq!(ids.len(), 1000);
        for id in ids.iter().take(20) {
            assert_eq!(id.len(), 36);
            assert_eq!(&id[14..15], "4");
            assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"));
            assert!(is_valid_session_id(id));
        }
    }

    #[test]
    fn namespace_is_per_api_instance_and_policy() {
        let a = namespace(Some("api-1"), Some("/hr"), "bridge.default");
        assert_eq!(a, "mcp-a2a-sessions/api-1/bridge.default");
        assert_ne!(a, namespace(Some("api-2"), Some("/hr"), "bridge.default"));
        assert_ne!(a, namespace(Some("api-1"), Some("/hr"), "other.default"));
        assert_eq!(namespace(None, Some("/hr"), "p"), "mcp-a2a-sessions//hr/p");
        assert_eq!(namespace(Some(""), None, "p"), "mcp-a2a-sessions/-/p");
    }

    #[test]
    fn ttl_is_clamped_and_matches_the_remote_backend() {
        assert_eq!(clamp_ttl(0), 60);
        assert_eq!(clamp_ttl(1800), 1800);
        assert_eq!(clamp_ttl(1_000_000), 86_400);
        assert_eq!(remote_ttl_millis(1800), 1_800_000);
        assert_eq!(remote_ttl_millis(1_000_000), 86_400_000);
    }

    #[test]
    fn expiry_follows_the_configured_ttl() {
        let entry = SessionEntry {
            context_id: None,
            task_id: None,
            touched_at: 1_000,
        };
        assert!(entry.is_live(1_000 + 1_799, 1_800));
        assert!(!entry.is_live(1_000 + 1_800, 1_800));
        // Lowering the TTL after the write expires the entry early.
        assert!(!entry.is_live(1_000 + 120, 60));
    }

    #[test]
    fn session_ids_must_be_visible_ascii_and_bounded() {
        assert!(is_valid_session_id("abc-123"));
        assert!(!is_valid_session_id(""));
        assert!(!is_valid_session_id("has space"));
        assert!(!is_valid_session_id("ünicode"));
        assert!(!is_valid_session_id(&"a".repeat(129)));
    }
}
