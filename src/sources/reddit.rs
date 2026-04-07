use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;
use serde_json;
use std::time::Duration;

const REDDIT_BASE: &str = "https://www.reddit.com";
const REDDIT_OAUTH_BASE: &str = "https://oauth.reddit.com";

pub const DEFAULT_SUBREDDITS: &[&str] = &[
    "programming",
    "technology",
    "MachineLearning",
    "artificial",
    "netsec",
    "cybersecurity",
    "opensource",
];

pub struct RedditSource {
    client: Client,
    base_url: String,
    oauth_base: String,
    pub min_score: u64,
}

#[derive(Debug, Clone)]
pub struct RedditPost {
    pub id: String,
    pub title: String,
    pub subreddit: String,
    pub score: u64,
    pub num_comments: u64,
    pub url: String,
    pub permalink: String,
    pub selftext: String,
}

// Reddit JSON structs ----------------------------------------------------------
#[derive(Deserialize)]
struct ListingResponse {
    data: ListingData,
}

#[derive(Deserialize)]
struct ListingData {
    children: Vec<Child>,
}

#[derive(Deserialize)]
struct Child {
    data: PostData,
}

#[derive(Deserialize)]
struct PostData {
    id: String,
    title: String,
    subreddit: String,
    score: i64,
    num_comments: u64,
    url: String,
    permalink: String,
    #[serde(default)]
    selftext: String,
}

impl RedditSource {
    pub fn new(min_score: u64) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            // Mimic the Reddit Android app to avoid 403 rate-limiting
            .user_agent("android:com.reddit.frontpage:v2023.21.0 (Linux; Android 13; SM-G991B)")
            .build()
            .unwrap_or_default();
        Self {
            client,
            base_url: REDDIT_BASE.to_string(),
            oauth_base: REDDIT_OAUTH_BASE.to_string(),
            min_score,
        }
    }

    /// Acquire an anonymous Reddit OAuth token using the installed_client grant.
    /// Requires REDDIT_CLIENT_ID env var. Returns None when not configured.
    async fn acquire_token(&self) -> Option<String> {
        let client_id = std::env::var("REDDIT_CLIENT_ID").ok()?;
        let device_id = format!(
            "{:016x}{:08x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            std::process::id()
        );
        let resp = self
            .client
            .post("https://www.reddit.com/api/v1/access_token")
            .basic_auth(&client_id, Some(""))
            .form(&[
                (
                    "grant_type",
                    "https://oauth.reddit.com/grants/installed_client",
                ),
                ("device_id", &device_id),
            ])
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let data: serde_json::Value = resp.json().await.ok()?;
        data["access_token"].as_str().map(String::from)
    }

    /// Fetch top posts from all tracked subreddits combined.
    /// If REDDIT_CLIENT_ID is set, uses OAuth (oauth.reddit.com) to bypass rate limits.
    /// Returns an empty Vec (not an error) on 403 so the watch loop keeps running.
    pub async fn fetch_hot(&self) -> Result<Vec<RedditPost>> {
        let multi = DEFAULT_SUBREDDITS.join("+");
        let token = self.acquire_token().await;
        let (base, url) = if token.is_some() {
            let u = format!("{}/r/{}/hot.json?limit=50", self.oauth_base, multi);
            (true, u)
        } else {
            let u = format!("{}/r/{}/hot.json?limit=50", self.base_url, multi);
            (false, u)
        };
        let mut builder = self.client.get(&url).header("Accept", "application/json");
        if let Some(ref t) = token {
            builder = builder.bearer_auth(t);
        }
        let resp = builder.send().await.context("Reddit API request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            // 403 = rate-limited by Reddit. Return empty instead of crashing the loop.
            if status.as_u16() == 403 || status.as_u16() == 429 {
                tracing::debug!(
                    oauth = base,
                    "Reddit hot feed {} — returning empty (rate-limited)",
                    status
                );
                return Ok(vec![]);
            }
            let snippet = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Reddit API HTTP {} — first 200 chars: {}",
                status,
                snippet.chars().take(200).collect::<String>()
            );
        }

        let body = resp.text().await.context("Reddit API body read failed")?;
        let raw: ListingResponse = serde_json::from_str(&body).map_err(|e| {
            let snippet: String = body.chars().take(200).collect();
            anyhow::anyhow!("Reddit JSON parse error: {} — body starts: {}", e, snippet)
        })?;

        let posts = raw
            .data
            .children
            .into_iter()
            .map(|c| {
                let d = c.data;
                let score = if d.score > 0 { d.score as u64 } else { 0 };
                RedditPost {
                    id: d.id,
                    title: d.title,
                    subreddit: d.subreddit,
                    score,
                    num_comments: d.num_comments,
                    url: d.url,
                    permalink: format!("https://reddit.com{}", d.permalink),
                    selftext: d.selftext,
                }
            })
            .filter(|p| p.score >= self.min_score)
            .collect();
        Ok(posts)
    }
}
