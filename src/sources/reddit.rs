use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;
use serde_json;
use std::time::Duration;

const REDDIT_BASE: &str = "https://www.reddit.com";

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
            // Reddit blocks empty/default User-Agents
            .user_agent("repo-radar/1.0 (+https://github.com/lemwaiping123-eng/repo-radar)")
            .build()
            .unwrap_or_default();
        Self {
            client,
            base_url: REDDIT_BASE.to_string(),
            min_score,
        }
    }

    /// Fetch top posts from all tracked subreddits combined.
    pub async fn fetch_hot(&self) -> Result<Vec<RedditPost>> {
        let multi = DEFAULT_SUBREDDITS.join("+");
        let url = format!("{}/r/{}/hot.json?limit=50", self.base_url, multi);
        let resp = self
            .client
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await
            .context("Reddit API request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
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
