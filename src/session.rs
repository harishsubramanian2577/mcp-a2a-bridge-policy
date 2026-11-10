// Copyright 2026 Salesforce, Inc. All rights reserved.
//! Optional server-side memory of the A2A conversation behind an MCP session. It is a fallback
//! for clients that do not echo `context_id`; the tool argument always wins.

use std::cell::Cell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{SystemTime, UNIX_EPOCH};

use pdk::data_storage::{
    DataStorage, DataStorageBuilder, LocalDataStorage, RemoteDataStorage, StoreMode,
};
use pdk::logger;
use serde::{Deserialize, Serialize};

const NAMESPACE: &str = "mcp-a2a-sessions";
/// Local storage has no TTL, so expired entries are swept every this many writes.
const LOCAL_SWEEP_INTERVAL: u32 = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEntry {
    pub context_id: String,
    /// Only set while the agent is waiting on the caller (input-required / auth-required).
    pub task_id: Option<String>,
    pub expires_at: u64,
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
    pub fn from_config(builder: &DataStorageBuilder, mode: &str, ttl_seconds: u64) -> Option<Self> {
        let backend = match mode {
            "local" => Backend::Local(builder.local(NAMESPACE)),
            "remote" => {
                let ttl_millis = u32::try_from(ttl_seconds.saturating_mul(1000)).unwrap_or(u32::MAX);
                Backend::Remote(builder.remote(NAMESPACE, ttl_millis))
            }
            _ => return None,
        };
        Some(SessionStore {
            backend,
            ttl_seconds,
            writes: Cell::new(0),
        })
    }

    pub async fn load(&self, key: &str) -> Option<SessionEntry> {
        let found = match &self.backend {
            Backend::Local(s) => s.get::<SessionEntry>(key).await,
            Backend::Remote(s) => s.get::<SessionEntry>(key).await,
        };
        match found {
            Ok(Some((entry, _))) if entry.expires_at > now_seconds() => Some(entry),
            Ok(_) => None,
            Err(err) => {
                // Fail open: losing the remembered context only starts a new conversation.
                logger::warn!("Session store read failed for '{}': {}", key, err);
                None
            }
        }
    }

    pub async fn save(&self, key: &str, context_id: String, task_id: Option<String>) {
        let entry = SessionEntry {
            context_id,
            task_id,
            expires_at: now_seconds() + self.ttl_seconds,
        };
        let stored = match &self.backend {
            Backend::Local(s) => s.store(key, &StoreMode::Always, &entry).await,
            Backend::Remote(s) => s.store(key, &StoreMode::Always, &entry).await,
        };
        if let Err(err) = stored {
            logger::warn!("Session store write failed for '{}': {}", key, err);
        }
        self.sweep_if_due().await;
    }

    pub async fn remove(&self, key: &str) {
        let removed = match &self.backend {
            Backend::Local(s) => s.delete(key).await,
            Backend::Remote(s) => s.delete(key).await,
        };
        if let Err(err) = removed {
            logger::warn!("Session store delete failed for '{}': {}", key, err);
        }
    }

    async fn sweep_if_due(&self) {
        let Backend::Local(storage) = &self.backend else {
            return;
        };
        let writes = self.writes.get().wrapping_add(1);
        self.writes.set(writes);
        if writes % LOCAL_SWEEP_INTERVAL != 0 {
            return;
        }
        let Ok(keys) = storage.get_keys().await else {
            return;
        };
        let now = now_seconds();
        for key in keys {
            if let Ok(Some((entry, _))) = storage.get::<SessionEntry>(&key).await {
                if entry.expires_at <= now {
                    let _ = storage.delete(&key).await;
                }
            }
        }
    }
}

/// Binds the stored conversation to the MCP session and, when configured, to a hash of the
/// caller identity header so a leaked session id alone cannot resume someone else's context.
/// `DefaultHasher::new()` uses fixed keys, so the hash is stable across replicas.
pub fn session_key(mcp_session_id: &str, identity: Option<&str>) -> String {
    match identity {
        Some(id) => {
            let mut hasher = DefaultHasher::new();
            id.hash(&mut hasher);
            format!("{}:{:016x}", mcp_session_id, hasher.finish())
        }
        None => mcp_session_id.to_string(),
    }
}

pub fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_key_binds_identity_deterministically() {
        assert_eq!(session_key("s1", None), "s1");
        let a = session_key("s1", Some("client-a"));
        assert_eq!(a, session_key("s1", Some("client-a")));
        assert_ne!(a, session_key("s1", Some("client-b")));
        assert!(a.starts_with("s1:"));
    }
}
