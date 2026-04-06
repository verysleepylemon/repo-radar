use anyhow::{Context, Result};
use reqwest::{Client, header};
use serde::Deserialize;
use tracing::debug;

use crate::config::Config;

const DEFAULT_API_BASE: &str = "https://api.github.com";

// ─── GitHub API structs ──────────────────────────────────────────────────────

#[derive(Debug, Deserialize, Clone)]
pub struct RepoInfo {
    pub full_name: String,
    pub description: Option<String>,
    pub language: Option<String>,
    pub stargazers_count: u64,
    pub forks_count: u64,
    pub html_url: String,
}

#[derive(Debug, Clone)]
pub struct TrendingEntry {
    pub full_name: String,
    pub stars: u64,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    items: Vec<SearchItem>,
}

#[derive(Debug, Deserialize)]
struct SearchItem {
    full_name: String,
    stargazers_count: u64,
}

#[derive(Debug, Clone)]
pub struct StarActivity {
    pub stars_now: u64,
    pub stars_gained_24h: u64,
}

// ─── Client ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct GitHubSource {
    client: Client,
    api_base: String,
}

impl GitHubSource {
    pub fn new(config: &Config) -> Result<Self> {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            "X-GitHub-Api-Version",
            header::HeaderValue::from_static("2022-11-28"),
        );
        if let Some(token) = &config.github_token {
            let auth = format!("Bearer {}", token);
            headers.insert(
                header::AUTHORIZATION,
                header::HeaderValue::from_str(&auth)?,
            );
        }

        let client = Client::builder()
            .user_agent("repo-radar/0.1")
            .default_headers(headers)
            .build()?;

        let api_base = config
            .github_api_base
            .clone()
            .unwrap_or_else(|| DEFAULT_API_BASE.to_string());

        Ok(Self { client, api_base })
    }

    pub async fn fetch_trending(&self, min_stars: u64, limit: u8) -> Result<Vec<TrendingEntry>> {
        let query = format!("stars:>{}", min_stars);
        let encoded = simple_encode(&query);
        let url = format!(
            "{}/search/repositories?q={}&sort=stars&order=desc&per_page={}",
            self.api_base, encoded, limit
        );
        debug!(%url, "Fetching trending repos");

        let resp: SearchResponse = self
            .client
            .get(&url)
            .send()
            .await
            .context("GitHub search request failed")?
            .error_for_status()
            .context("GitHub search returned non-2xx")?
            .json()
            .await
            .context("Failed to parse GitHub search response")?;

        Ok(resp
            .items
            .into_iter()
            .map(|i| TrendingEntry {
                full_name: i.full_name,
                stars: i.stargazers_count,
            })
            .collect())
    }

    pub async fn fetch_repo_info(&self, full_name: &str) -> Result<RepoInfo> {
        let url = format!("{}/repos/{}", self.api_base, full_name);
        debug!(%url, "Fetching repo info");

        let info: RepoInfo = self
            .client
            .get(&url)
            .send()
            .await
            .context("GitHub repo info request failed")?
            .error_for_status()
            .context("GitHub repo info returned non-2xx")?
            .json()
            .await
            .context("Failed to parse repo info")?;

        Ok(info)
    }

    pub async fn fetch_star_activity(&self, full_name: &str) -> Result<StarActivity> {
        let repo_info = self.fetch_repo_info(full_name).await?;
        let stars_now = repo_info.stargazers_count;

        let cutoff = chrono::Utc::now() - chrono::Duration::hours(24);

        #[derive(Deserialize)]
        struct Event {
            #[serde(rename = "type")]
            event_type: String,
            created_at: String,
        }

        let mut stars_gained: u64 = 0;
        for page in 1u8..=3 {
            let url = format!(
                "{}/repos/{}/events?per_page=100&page={}",
                self.api_base, full_name, page
            );
            let events: Vec<Event> = match self
                .client
                .get(&url)
                .send()
                .await
                .and_then(|r| r.error_for_status())
            {
                Ok(resp) => resp.json().await.unwrap_or_default(),
                Err(_) => break,
            };

            if events.is_empty() {
                break;
            }

            for evt in &events {
                if evt.event_type != "WatchEvent" {
                    continue;
                }
                if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&evt.created_at) {
                    if ts > cutoff {
                        stars_gained += 1;
                    }
                }
            }
        }

        Ok(StarActivity {
            stars_now,
            stars_gained_24h: stars_gained,
        })
    }
}

fn simple_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            c if c.is_ascii_alphanumeric() => out.push(c),
            '-' | '_' | '.' | '~' => out.push(c),
            ' ' => out.push('+'),
            c => {
                for b in c.to_string().into_bytes() {
                    out.push_str(&format!("%{:02X}", b));
                }
            }
        }
    }
    out
}
