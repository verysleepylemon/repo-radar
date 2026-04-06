use anyhow::{Context, Result};
use reqwest::Client;
use serde::Deserialize;
use tracing::debug;

const DEFAULT_HN_BASE: &str = "https://hn.algolia.com/api/v1";

#[derive(Debug, Deserialize)]
struct AlgoliaResponse {
    hits: Vec<Hit>,
}

#[derive(Debug, Deserialize)]
struct Hit {
    #[serde(rename = "objectID")]
    object_id: String,
    title: Option<String>,
    url: Option<String>,
    points: Option<u64>,
    #[serde(rename = "num_comments")]
    num_comments: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct HnStory {
    pub id: String,
    pub title: String,
    pub url: Option<String>,
    pub points: u64,
    pub num_comments: u64,
}

#[derive(Debug, Clone)]
pub struct HackerNewsSource {
    client: Client,
    api_base: String,
}

impl HackerNewsSource {
    pub fn new() -> Self {
        Self::new_with_base_url(DEFAULT_HN_BASE)
    }

    /// Constructor with custom base URL — used in integration tests via mockito.
    pub fn new_with_base_url(base_url: &str) -> Self {
        let client = Client::builder()
            .user_agent("repo-radar/0.1")
            .build()
            .expect("Failed to build HTTP client");
        Self {
            client,
            api_base: base_url.trim_end_matches('/').to_string(),
        }
    }

    pub async fn fetch_hot_show_hn(&self, limit: u8) -> Result<Vec<HnStory>> {
        let cutoff = chrono::Utc::now() - chrono::Duration::hours(24);
        let timestamp = cutoff.timestamp();

        let url = format!(
            "{}/search_by_date?tags=show_hn&numericFilters=created_at_i>{}&hitsPerPage={}",
            self.api_base, timestamp, limit
        );
        debug!(%url, "Fetching Show HN stories");

        let resp: AlgoliaResponse = self
            .client
            .get(&url)
            .send()
            .await
            .context("HN Algolia request failed")?
            .error_for_status()
            .context("HN Algolia returned non-2xx")?
            .json()
            .await
            .context("Failed to parse HN Algolia response")?;

        Ok(resp
            .hits
            .into_iter()
            .map(|h| HnStory {
                id: h.object_id,
                title: h.title.unwrap_or_default(),
                url: h.url,
                points: h.points.unwrap_or(0),
                num_comments: h.num_comments.unwrap_or(0),
            })
            .collect())
    }
}

impl Default for HackerNewsSource {
    fn default() -> Self {
        Self::new()
    }
}
