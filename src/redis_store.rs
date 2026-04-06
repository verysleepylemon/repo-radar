use anyhow::Result;
use redis::{aio::ConnectionManager, AsyncCommands, Client};
use serde_json;
use std::time::Duration;
use tracing::{debug, warn};

use crate::detector::Alert;

const ALERTS_LIST_KEY: &str = "repo-radar:alerts";
const CHANNEL_KEY: &str = "repo-radar:events";
const MAX_ALERTS: u64 = 500;

/// Thin wrapper around Redis for deduplication, alert storage, and pub/sub.
#[derive(Clone)]
pub struct RedisStore {
    manager: ConnectionManager,
}

impl RedisStore {
    pub async fn connect(url: &str) -> Result<Self> {
        let client = Client::open(url)?;
        let manager = ConnectionManager::new(client).await?;
        Ok(Self { manager })
    }

    /// Connect to Redis, returning `None` (with a warning) if unavailable.
    /// This allows repo-radar to run without Redis installed.
    pub async fn try_connect(url: &str) -> Option<Self> {
        match Self::connect(url).await {
            Ok(store) => Some(store),
            Err(e) => {
                warn!(error = %e, "Redis unavailable — running with in-memory dedup only");
                None
            }
        }
    }

    /// Check if a dedup key was already seen (returns false on Redis errors to avoid blocking).
    pub async fn is_seen(&self, key: &str) -> Result<bool> {
        let mut conn = self.manager.clone();
        let full_key = format!("repo-radar:seen:{}", key);
        let exists: bool = conn.exists(&full_key).await?;
        Ok(exists)
    }

    /// Mark a key as seen with a TTL.
    pub async fn mark_seen(&self, key: &str, ttl: Duration) -> Result<()> {
        let mut conn = self.manager.clone();
        let full_key = format!("repo-radar:seen:{}", key);
        let ttl_secs = ttl.as_secs();
        conn.set_ex::<_, _, ()>(&full_key, 1u8, ttl_secs).await?;
        debug!(key = %full_key, ttl_secs, "Marked as seen");
        Ok(())
    }

    /// Persist an alert to the Redis list (capped at MAX_ALERTS).
    pub async fn save_alert(&self, alert: &Alert) -> Result<()> {
        let mut conn = self.manager.clone();
        let json = serde_json::to_string(alert)?;
        conn.lpush::<_, _, ()>(ALERTS_LIST_KEY, &json).await?;
        // Trim to keep list from growing unbounded
        conn.ltrim::<_, ()>(ALERTS_LIST_KEY, 0, MAX_ALERTS as isize - 1)
            .await?;
        Ok(())
    }

    /// Publish an alert to the Redis pub/sub channel for downstream subscribers.
    pub async fn publish_alert(&self, alert: &Alert) -> Result<()> {
        let mut conn = self.manager.clone();
        let json = serde_json::to_string(alert)?;
        let receivers: i64 = conn.publish(CHANNEL_KEY, &json).await?;
        debug!(receivers, "Published alert to Redis channel");
        Ok(())
    }

    /// Retrieve the N most recent alerts from the list.
    pub async fn get_recent_alerts(&self, n: u64) -> Result<Vec<Alert>> {
        let mut conn = self.manager.clone();
        let items: Vec<String> = conn.lrange(ALERTS_LIST_KEY, 0, n as isize - 1).await?;
        let alerts: Vec<Alert> = items
            .iter()
            .filter_map(|s| match serde_json::from_str(s) {
                Ok(a) => Some(a),
                Err(e) => {
                    warn!(error = %e, "Failed to deserialize alert");
                    None
                }
            })
            .collect();
        Ok(alerts)
    }

    /// Read up to `count` raw JSON strings from any Redis list key.
    /// Used by the web layer to fetch Python-scanner findings from
    /// `repo-radar:secrets` without exposing `manager` publicly.
    pub async fn get_raw_list(&self, key: &str, count: isize) -> Result<Vec<String>> {
        let mut conn = self.manager.clone();
        let items: Vec<String> = conn.lrange(key, 0, count - 1).await?;
        Ok(items)
    }
}
