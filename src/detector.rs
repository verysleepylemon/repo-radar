use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::notifiers::NotifierSet;
use crate::redis_store::RedisStore;
use crate::sources::github::GitHubSource;
use crate::sources::hackernews::HackerNewsSource;

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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AlertSource {
    GitHubTrending,
    HackerNews,
    SpikeDetected,
}

impl fmt::Display for AlertSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AlertSource::GitHubTrending => write!(f, "GitHub Trending"),
            AlertSource::HackerNews => write!(f, "Hacker News"),
            AlertSource::SpikeDetected => write!(f, "Spike Detected"),
        }
    }
}

/// Core detection logic — wraps config, Redis store, and notifiers.
#[derive(Clone)]
pub struct Detector {
    config: Config,
    store: RedisStore,
    notifiers: NotifierSet,
}

impl Detector {
    pub fn new(config: Config, store: RedisStore, notifiers: NotifierSet) -> Self {
        Self {
            config,
            store,
            notifiers,
        }
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
            if self.store.is_seen(&dedup_key).await.unwrap_or(false) {
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
            };

            if let Err(e) = self.fire_alert(&alert).await {
                warn!(error = %e, "Failed to fire HN alert");
            }

            self.store
                .mark_seen(&dedup_key, self.config.dedup_ttl())
                .await?;
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
        if self.store.is_seen(&dedup_key).await.unwrap_or(false) {
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
        }))
    }

    async fn fire_alert(&self, alert: &Alert) -> Result<()> {
        self.store.save_alert(alert).await?;
        let dedup_key = format!("spike:{}", alert.repo_full_name);
        self.store
            .mark_seen(&dedup_key, self.config.dedup_ttl())
            .await?;
        self.store.publish_alert(alert).await?;
        self.notifiers.notify(alert).await;
        Ok(())
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
