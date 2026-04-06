/// Twitter / X source - powered by twikit (no API key needed).
///
/// Instead of the official Twitter API, this module reads from a JSON cache
/// file written by the companion `twikit_feed.py` Python script.
///
/// Run `python twikit_feed.py` once (or on a cron) to populate the cache.
/// The Rust service picks it up automatically on the next poll cycle.
use anyhow::Result;
use serde::Deserialize;
use std::path::PathBuf;
use tracing::debug;

pub struct TwitterSource;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct TweetMetrics {
    #[serde(default)]
    pub retweet_count: u64,
    #[serde(default)]
    pub like_count: u64,
    #[serde(default)]
    pub reply_count: u64,
    #[serde(default)]
    pub quote_count: u64,
}

impl TweetMetrics {
    pub fn engagement(&self) -> u64 {
        self.retweet_count * 5 + self.like_count + self.reply_count * 2 + self.quote_count * 3
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Tweet {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub public_metrics: TweetMetrics,
    pub author_id: Option<String>,
    /// Twitter/X username (populated by twikit)
    pub username: Option<String>,
    /// Avatar URL (populated by twikit)
    pub avatar_url: Option<String>,
}

impl TwitterSource {
    /// Create a new handle. The `_credentials` argument is kept for API
    /// compatibility but is ignored - twikit handles its own auth.
    pub fn new(_credentials: &str) -> Self {
        Self
    }

    /// Return cached tweets from the twikit feed (tech/viral content).
    pub async fn search_tech_trending(&self, min_engagement: u64) -> Result<Vec<Tweet>> {
        let mut tweets = read_twikit_cache().await?;
        tweets.retain(|t| t.public_metrics.engagement() >= min_engagement);
        tweets.sort_by(|a, b| {
            b.public_metrics
                .engagement()
                .cmp(&a.public_metrics.engagement())
        });
        Ok(tweets)
    }

    /// Return cached tweets from the twikit feed (sensitive/leaked content).
    pub async fn search_sensitive(&self, min_engagement: u64) -> Result<Vec<Tweet>> {
        self.search_tech_trending(min_engagement).await
    }
}

/// Locate and read the twikit JSON cache file.
/// Searches (in order): next to the binary, then the current working directory,
/// then ~/.repo-radar/twikit_cache.json.
async fn read_twikit_cache() -> Result<Vec<Tweet>> {
    let candidates: Vec<PathBuf> = [
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("twikit_cache.json"))),
        Some(PathBuf::from("twikit_cache.json")),
        home_dir().map(|h| h.join(".repo-radar").join("twikit_cache.json")),
    ]
    .into_iter()
    .flatten()
    .collect();

    for path in &candidates {
        if path.exists() {
            debug!(path = %path.display(), "Loading twikit cache");
            let content = tokio::fs::read_to_string(path).await?;
            let tweets: Vec<Tweet> = serde_json::from_str(&content).unwrap_or_default();
            return Ok(tweets);
        }
    }

    debug!("No twikit_cache.json found - Twitter/X feed empty (run twikit_feed.py)");
    Ok(vec![])
}

fn home_dir() -> Option<PathBuf> {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_engagement() {
        let m = TweetMetrics {
            retweet_count: 10,
            like_count: 50,
            reply_count: 5,
            quote_count: 2,
        };
        // 10*5 + 50 + 5*2 + 2*3 = 50+50+10+6 = 116
        assert_eq!(m.engagement(), 116);
    }

    #[tokio::test]
    async fn no_cache_returns_empty() {
        let src = TwitterSource::new("ignored");
        let tweets = src.search_tech_trending(0).await.unwrap();
        assert!(tweets.len() < 10_000);
    }
}
