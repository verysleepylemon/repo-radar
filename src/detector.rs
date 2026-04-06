use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::notifiers::{windows_toast, NotifierSet};
use crate::redis_store::RedisStore;
use crate::sources::github::GitHubSource;
use crate::sources::hackernews::HackerNewsSource;
use crate::sources::reddit::RedditSource;
use crate::sources::rss::RssSource;
use crate::sources::twitter::TwitterSource;
use crate::web::{push_alert, AlertBuf};

/// Priority level for an alert — drives notification urgency.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum AlertPriority {
    #[default]
    Normal,
    High,
    /// Critical: sensitive / censored / leaked content detected.
    Critical,
}

/// A detected trend spike alert.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alert {
    pub repo_full_name: String,
    pub description: Option<String>,
    pub language: Option<String>,
    pub stars_now: u64,
    pub stars_gained_24h: u64,
    pub forks: u64,
    pub growth_factor: f64,
    pub score: f64,
    pub detected_at: DateTime<Utc>,
    pub source: AlertSource,
    pub url: String,
    #[serde(default)]
    pub priority: AlertPriority,
}

impl Alert {
    /// Returns true if this alert was triggered by sensitive/censored content.
    pub fn is_critical(&self) -> bool {
        self.priority == AlertPriority::Critical
    }

    /// Returns true if the alert originates from GitHub.
    pub fn is_github(&self) -> bool {
        matches!(
            self.source,
            AlertSource::GitHubTrending | AlertSource::SpikeDetected
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AlertSource {
    GitHubTrending,
    HackerNews,
    SpikeDetected,
    Twitter,
    Reddit,
    RssFeed(String),
}

impl fmt::Display for AlertSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AlertSource::GitHubTrending => write!(f, "GitHub Trending"),
            AlertSource::HackerNews => write!(f, "Hacker News"),
            AlertSource::SpikeDetected => write!(f, "Spike Detected"),
            AlertSource::Twitter => write!(f, "Twitter / X"),
            AlertSource::Reddit => write!(f, "Reddit"),
            AlertSource::RssFeed(name) => write!(f, "RSS: {}", name),
        }
    }
}

/// Keywords that indicate censored, leaked, or suppressed content.
const SENSITIVE_KEYWORDS: &[&str] = &[
    "leaked",
    "banned",
    "censored",
    "removed",
    "dmca",
    "takedown",
    "zero-day",
    "0day",
    "backdoor",
    "whistleblower",
    "classified",
    "suppressed",
    "deplatformed",
    "surveillance",
    "breach",
    "exploit",
    "arrested",
    "seized",
    "shutdown",
    "wiped",
];

/// Returns true if `text` contains any sensitive keyword.
pub fn is_sensitive(text: &str) -> bool {
    let lower = text.to_lowercase();
    SENSITIVE_KEYWORDS.iter().any(|kw| lower.contains(kw))
}

/// Core detection logic — wraps config, optional Redis store, and notifiers.
#[derive(Clone)]
pub struct Detector {
    config: Config,
    store: Option<RedisStore>,
    notifiers: NotifierSet,
    /// Optional shared web-dashboard alert buffer.
    alert_buf: Option<AlertBuf>,
}

impl Detector {
    pub fn new(config: Config, store: Option<RedisStore>, notifiers: NotifierSet) -> Self {
        Self {
            config,
            store,
            notifiers,
            alert_buf: None,
        }
    }

    /// Attach a shared alert buffer so the web dashboard stays live.
    pub fn with_alert_buf(mut self, buf: AlertBuf) -> Self {
        self.alert_buf = Some(buf);
        self
    }

    /// Scan GitHub and check each trending repo for spike signals.
    pub async fn scan_github(&self, source: &GitHubSource) -> Result<()> {
        info!("Scanning GitHub trending...");
        let trending = source.fetch_trending(self.config.min_stars, 30).await?;
        info!(count = trending.len(), "Fetched trending repos");

        for entry in &trending {
            match self.check_repo(source, &entry.full_name).await {
                Ok(Some(alert)) => {
                    info!(
                        repo = %alert.repo_full_name,
                        stars_gained = alert.stars_gained_24h,
                        score = alert.score,
                        "Spike detected"
                    );
                    if let Err(e) = self.fire_alert(&alert).await {
                        warn!(error = %e, "Failed to fire alert");
                    }
                }
                Ok(None) => debug!(repo = %entry.full_name, "No spike"),
                Err(e) => warn!(repo = %entry.full_name, error = %e, "Error checking repo"),
            }
        }

        Ok(())
    }

    /// Scan HackerNews "Show HN" posts for viral developer tools.
    pub async fn scan_hackernews(&self, source: &HackerNewsSource) -> Result<()> {
        info!("Scanning HackerNews...");
        let stories = source.fetch_hot_show_hn(50).await?;

        for story in &stories {
            // Extract GitHub repo links from HN posts
            let github_url = match story.url.as_deref().and_then(extract_github_repo) {
                Some(u) => u,
                None => continue,
            };

            // Deduplicate — skip if we already alerted on this HN story
            let dedup_key = format!("hn:{}", story.id);
            if self.is_seen(&dedup_key).await {
                continue;
            }

            info!(
                story_id = %story.id,
                title = %story.title,
                points = story.points,
                github = %github_url,
                "HN: GitHub repo going viral"
            );

            let alert = Alert {
                repo_full_name: github_url.clone(),
                description: Some(story.title.clone()),
                language: None,
                stars_now: 0,
                stars_gained_24h: 0,
                forks: 0,
                growth_factor: 0.0,
                score: story.points as f64,
                detected_at: Utc::now(),
                source: AlertSource::HackerNews,
                url: format!("https://news.ycombinator.com/item?id={}", story.id),
                priority: AlertPriority::Normal,
            };

            if let Err(e) = self.fire_alert(&alert).await {
                warn!(error = %e, "Failed to fire HN alert");
            }

            self.mark_seen(&dedup_key).await;
        }

        Ok(())
    }

    /// Check a single repo for spike signals. Returns Some(Alert) if spiking.
    pub async fn check_repo(&self, source: &GitHubSource, repo: &str) -> Result<Option<Alert>> {
        let info = match source.fetch_repo_info(repo).await {
            Ok(i) => i,
            Err(e) => {
                debug!(repo = %repo, error = %e, "Could not fetch repo info");
                return Ok(None);
            }
        };

        if info.stargazers_count < self.config.min_stars {
            return Ok(None);
        }

        let activity = source.fetch_star_activity(repo).await?;
        let stars_24h = activity.stars_gained_24h;

        if stars_24h < self.config.spike_threshold {
            return Ok(None);
        }

        // Dedup — don't fire twice for the same repo in the dedup window
        let dedup_key = format!("spike:{}", repo);
        if self.is_seen(&dedup_key).await {
            debug!(repo = %repo, "Already alerted recently");
            return Ok(None);
        }

        let growth_factor = if info.stargazers_count > stars_24h {
            (stars_24h as f64) / ((info.stargazers_count - stars_24h) as f64).max(1.0)
        } else {
            stars_24h as f64
        };

        let score = stars_24h as f64 + (growth_factor * 100.0);

        Ok(Some(Alert {
            repo_full_name: repo.to_string(),
            description: info.description,
            language: info.language,
            stars_now: info.stargazers_count,
            stars_gained_24h: stars_24h,
            forks: info.forks_count,
            growth_factor,
            score,
            detected_at: Utc::now(),
            source: AlertSource::SpikeDetected,
            url: info.html_url,
            priority: AlertPriority::Normal,
        }))
    }

    /// Scan Twitter/X for viral tech posts and sensitive content.
    pub async fn scan_twitter(&self, source: &TwitterSource) -> Result<()> {
        info!("Scanning Twitter/X...");
        let min_engagement = 50;

        // Trending tech tweets
        let trending = source
            .search_tech_trending(min_engagement)
            .await
            .unwrap_or_default();
        for tweet in trending.iter().take(10) {
            let key = format!("twitter:{}", tweet.id);
            if self.is_seen(&key).await {
                continue;
            }
            let priority = if is_sensitive(&tweet.text) {
                AlertPriority::Critical
            } else {
                AlertPriority::Normal
            };
            let alert = Alert {
                repo_full_name: format!("@tweet:{}", &tweet.id[..tweet.id.len().min(12)]),
                description: Some(tweet.text.chars().take(280).collect()),
                language: Some("twitter".to_string()),
                stars_now: tweet.public_metrics.like_count,
                stars_gained_24h: tweet.public_metrics.quote_count,
                forks: tweet.public_metrics.retweet_count,
                growth_factor: 0.0,
                score: tweet.public_metrics.engagement() as f64,
                detected_at: Utc::now(),
                source: AlertSource::Twitter,
                url: format!("https://twitter.com/i/web/status/{}", tweet.id),
                priority,
            };
            if let Err(e) = self.fire_alert(&alert).await {
                warn!(error = %e, "Failed to fire Twitter alert");
            }
            self.mark_seen(&key).await;
        }

        // Sensitive content sweep
        let sensitive = source.search_sensitive(20).await.unwrap_or_default();
        for tweet in sensitive.iter().take(5) {
            let key = format!("twitter-s:{}", tweet.id);
            if self.is_seen(&key).await {
                continue;
            }
            let alert = Alert {
                repo_full_name: format!("🚨 sensitive:{}", &tweet.id[..tweet.id.len().min(8)]),
                description: Some(tweet.text.chars().take(280).collect()),
                language: Some("twitter".to_string()),
                stars_now: tweet.public_metrics.like_count,
                stars_gained_24h: tweet.public_metrics.quote_count,
                forks: tweet.public_metrics.retweet_count,
                growth_factor: 0.0,
                score: tweet.public_metrics.engagement() as f64,
                detected_at: Utc::now(),
                source: AlertSource::Twitter,
                url: format!("https://twitter.com/i/web/status/{}", tweet.id),
                priority: AlertPriority::Critical,
            };
            if let Err(e) = self.fire_alert(&alert).await {
                warn!(error = %e, "Failed to fire sensitive Twitter alert");
            }
            self.mark_seen(&key).await;
        }

        Ok(())
    }

    /// Scan Reddit for viral tech posts.
    pub async fn scan_reddit(&self, source: &RedditSource) -> Result<()> {
        info!("Scanning Reddit...");
        let posts = source.fetch_hot().await?;
        for post in posts.iter().take(15) {
            let key = format!("reddit:{}", post.id);
            if self.is_seen(&key).await {
                continue;
            }
            let body_text = format!("{} {}", post.title, post.selftext);
            let priority = if is_sensitive(&body_text) {
                AlertPriority::Critical
            } else {
                AlertPriority::Normal
            };
            let alert = Alert {
                repo_full_name: format!(
                    "r/{}: {}",
                    post.subreddit,
                    post.title.chars().take(60).collect::<String>()
                ),
                description: Some(post.title.clone()),
                language: Some(format!("reddit/r/{}", post.subreddit)),
                stars_now: post.score,
                stars_gained_24h: 0,
                forks: post.num_comments,
                growth_factor: 0.0,
                score: (post.score as f64) + (post.num_comments as f64 * 0.5),
                detected_at: Utc::now(),
                source: AlertSource::Reddit,
                url: post.permalink.clone(),
                priority,
            };
            if let Err(e) = self.fire_alert(&alert).await {
                warn!(error = %e, "Failed to fire Reddit alert");
            }
            self.mark_seen(&key).await;
        }
        Ok(())
    }

    /// Scan RSS/Atom feeds for tech news and sensitive content.
    pub async fn scan_rss(&self, source: &RssSource) -> Result<()> {
        info!("Scanning RSS feeds...");
        let items = source.fetch_all().await;
        for item in &items {
            let key = format!("rss:{}", item.link);
            if self.is_seen(&key).await {
                continue;
            }
            let combined = format!("{} {}", item.title, item.description);
            let priority = if is_sensitive(&combined) {
                AlertPriority::Critical
            } else {
                AlertPriority::Normal
            };
            let alert = Alert {
                repo_full_name: item.title.chars().take(80).collect(),
                description: Some(item.description.chars().take(300).collect()),
                language: Some(item.feed_name.clone()),
                stars_now: 0,
                stars_gained_24h: 0,
                forks: 0,
                growth_factor: 0.0,
                score: if priority == AlertPriority::Critical {
                    1000.0
                } else {
                    200.0
                },
                detected_at: item.published.unwrap_or_else(Utc::now),
                source: AlertSource::RssFeed(item.feed_name.clone()),
                url: item.link.clone(),
                priority,
            };
            if let Err(e) = self.fire_alert(&alert).await {
                warn!(error = %e, "Failed to fire RSS alert");
            }
            self.mark_seen(&key).await;
        }
        Ok(())
    }

    async fn fire_alert(&self, alert: &Alert) -> Result<()> {
        if let Some(ref s) = self.store {
            let _ = s.save_alert(alert).await;
            let dedup_key = format!("spike:{}", alert.repo_full_name);
            let _ = s.mark_seen(&dedup_key, self.config.dedup_ttl()).await;
            let _ = s.publish_alert(alert).await;
        }
        // Push to web dashboard buffer if one is attached.
        if let Some(ref buf) = self.alert_buf {
            push_alert(buf, alert.clone()).await;
        }
        // Windows toast for Critical priority
        if alert.is_critical() {
            let title = format!(
                "🚨 SENSITIVE: {}",
                &alert.repo_full_name.chars().take(60).collect::<String>()
            );
            let raw_body = alert.description.as_deref().unwrap_or("");
            let body = if raw_body.is_empty() {
                "Sensitive content detected"
            } else {
                raw_body
            };
            windows_toast::notify(&title, body);
        }
        self.notifiers.notify(alert).await;
        Ok(())
    }

    /// Check if a dedup key was seen. Returns false if Redis is unavailable.
    async fn is_seen(&self, key: &str) -> bool {
        match &self.store {
            Some(s) => s.is_seen(key).await.unwrap_or(false),
            None => false,
        }
    }

    /// Mark a key as seen. Silent no-op if Redis is unavailable.
    async fn mark_seen(&self, key: &str) {
        if let Some(ref s) = self.store {
            let _ = s.mark_seen(key, self.config.dedup_ttl()).await;
        }
    }
}

fn extract_github_repo(url: &str) -> Option<String> {
    let url = url.trim_end_matches('/');
    let prefix = "https://github.com/";
    if let Some(rest) = url.strip_prefix(prefix) {
        let parts: Vec<&str> = rest.splitn(3, '/').collect();
        if parts.len() >= 2 && !parts[0].is_empty() && !parts[1].is_empty() {
            return Some(format!("{}/{}", parts[0], parts[1]));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_github_repo_standard() {
        assert_eq!(
            extract_github_repo("https://github.com/owner/repo"),
            Some("owner/repo".to_string())
        );
    }

    #[test]
    fn test_extract_github_repo_trailing_slash() {
        assert_eq!(
            extract_github_repo("https://github.com/owner/repo/"),
            Some("owner/repo".to_string())
        );
    }

    #[test]
    fn test_extract_github_repo_with_subpath() {
        assert_eq!(
            extract_github_repo("https://github.com/owner/repo/blob/main/README.md"),
            Some("owner/repo".to_string())
        );
    }

    #[test]
    fn test_extract_github_repo_non_github() {
        assert_eq!(extract_github_repo("https://crates.io/crates/tokio"), None);
    }

    #[test]
    fn test_extract_github_repo_no_repo() {
        assert_eq!(extract_github_repo("https://github.com/owner"), None);
    }
}
