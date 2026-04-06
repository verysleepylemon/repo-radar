use anyhow::{Context, Result};
use reqwest::{header, Client};
use serde::Deserialize;
use std::time::Duration;
use tracing::debug;

const TWITTER_API_BASE: &str = "https://api.twitter.com/2";

pub struct TwitterSource {
    client: Client,
    bearer_token: String,
    api_base: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Tweet {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub public_metrics: TweetMetrics,
    pub author_id: Option<String>,
}

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
    /// Weighted engagement score: retweets × 5 + likes + replies × 2 + quotes × 3.
    pub fn engagement(&self) -> u64 {
        self.retweet_count * 5 + self.like_count + self.reply_count * 2 + self.quote_count * 3
    }
}

#[derive(Deserialize)]
struct SearchResponse {
    data: Option<Vec<Tweet>>,
}

impl TwitterSource {
    pub fn new(bearer_token: &str) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        Self {
            client,
            bearer_token: bearer_token.to_string(),
            api_base: TWITTER_API_BASE.to_string(),
        }
    }

    #[cfg(test)]
    pub fn new_with_base_url(bearer_token: &str, base_url: &str) -> Self {
        let mut s = Self::new(bearer_token);
        s.api_base = base_url.to_string();
        s
    }

    /// Search for viral tech discussions on X/Twitter.
    pub async fn search_tech_trending(&self, min_engagement: u64) -> Result<Vec<Tweet>> {
        let query = "(github OR #programming OR #AI OR #opensource OR #rustlang \
                     OR #kubernetes) -is:retweet lang:en has:links";
        self.search(query, 100, min_engagement).await
    }

    /// Search specifically for sensitive/censored tech content.
    pub async fn search_sensitive(&self, min_engagement: u64) -> Result<Vec<Tweet>> {
        let query = "(leaked OR censored OR banned OR \"taken down\" OR dmca \
                     OR \"zero-day\" OR \"data breach\" OR whistleblower) \
                     (github OR tech OR developer OR security OR AI) \
                     -is:retweet lang:en";
        self.search(query, 50, min_engagement).await
    }

    async fn search(&self, query: &str, max: u32, min_engagement: u64) -> Result<Vec<Tweet>> {
        let url = format!("{}/tweets/search/recent", self.api_base);
        let response = self
            .client
            .get(&url)
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", self.bearer_token),
            )
            .query(&[
                ("query", query),
                ("max_results", &max.clamp(10, 100).to_string()),
                ("tweet.fields", "public_metrics,author_id,created_at"),
            ])
            .send()
            .await
            .context("Twitter API request failed")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            debug!(status=%status, body=%body, "Twitter API error");
            anyhow::bail!("Twitter API returned {}", status);
        }

        let data: SearchResponse = response.json().await?;
        let mut tweets = data.data.unwrap_or_default();
        tweets.retain(|t| t.public_metrics.engagement() >= min_engagement);
        tweets.sort_by(|a, b| {
            b.public_metrics
                .engagement()
                .cmp(&a.public_metrics.engagement())
        });
        Ok(tweets)
    }
}
