//! Web dashboard server for repo-radar.
//!
//! Serves a dark-mode HTML dashboard at `/` and three JSON APIs:
//! - `/api/alerts`   — recent alerts detected by the watch loop
//! - `/api/vip`      — AI/tech stories from HN, filtered by VIP names & keywords
//! - `/api/trending` — recently-popular GitHub repos (last 30 days)

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use axum::{
    extract::{Path, Query, State},
    response::{Html, Json},
    routing::get,
    Router,
};
use chrono::{Duration as CDuration, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;

use crate::detector::Alert;
use crate::replicator::{new_seen_replications, Replicator, SeenReplications};
use crate::secret_scanner::FindingsBuf;

/// Type-alias for the shared cache slots used in WebState.
type Cache<T> = Arc<RwLock<Option<(Instant, T)>>>;

pub const MAX_ALERT_BUF: usize = 100;

/// Thread-safe circular buffer of recent alerts shared between watch loop and
/// the web server.
pub type AlertBuf = Arc<RwLock<VecDeque<Alert>>>;

/// Create a new, empty alert buffer.
pub fn new_alert_buf() -> AlertBuf {
    Arc::new(RwLock::new(VecDeque::with_capacity(MAX_ALERT_BUF)))
}

/// Push an alert into the shared buffer, dropping the oldest when full.
pub async fn push_alert(buf: &AlertBuf, alert: Alert) {
    let mut g = buf.write().await;
    g.push_front(alert);
    if g.len() > MAX_ALERT_BUF {
        g.pop_back();
    }
}

// ─── VIP / keyword filter ─────────────────────────────────────────────────────

/// Well-known AI/tech people — matched case-insensitively against story titles
/// and URLs coming from HN.
const VIP_TERMS: &[&str] = &[
    "karpathy",
    "andrej karpathy",
    "sam altman",
    "yann lecun",
    "ylecun",
    "dario amodei",
    "ilya sutskever",
    "jeremy howard",
    "fast.ai",
    "francois chollet",
    "sebastian raschka",
    "harrison chase",
    "langchain",
    "chaofan shou",
    "george hotz",
    "geohot",
    "pieter abbeel",
    "chris olah",
    "noam shazeer",
    "greg brockman",
    "demis hassabis",
    "emad mostaque",
    "geoffrey hinton",
    // AI product names that signal leak/release events
    "claude code",
    "claude-code",
    "claw-code",
    "claudecode",
    "anthropic leak",
    "openai codex leak",
    "gemini cli",
];

/// General AI / coding keywords — used to keep the feed on-topic.
const AI_KEYWORDS: &[&str] = &[
    " llm",
    "language model",
    "large model",
    "foundation model",
    "claude",
    "gpt",
    "chatgpt",
    "openai",
    "anthropic",
    "deepmind",
    "gemini",
    "mistral",
    "llama",
    "phi-",
    "qwen",
    "deepseek",
    "transformer",
    "diffusion",
    "neural net",
    "machine learning",
    " ml ",
    " ai ",
    "code generation",
    "copilot",
    "cursor ai",
    "devin",
    " agent",
    "agentic",
    "rag",
    "fine-tun",
    "finetuning",
    "training run",
    "inference",
    "benchmark",
    "evals",
    "evaluation",
    "source map",
    "npm leak",
    "source code leaked",
    "sourcescontent",
    "bun sourcemap",
    ".map files",
    "source maps leaked",
    "npmignore",
    "revealed source",
];

// ─── Shared state ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct WebState {
    pub alerts: AlertBuf,
    pub findings: FindingsBuf,
    pub http: reqwest::Client,
    pub redis_store: Option<crate::redis_store::RedisStore>,
    pub replicator: Arc<Replicator>,
    pub seen_repls: SeenReplications,
    vip_cache: Cache<Vec<VipItem>>,
    social_cache: Cache<Vec<VipItem>>,
    trend_cache: Cache<serde_json::Value>,
    leak_cache: Cache<Vec<LeakItem>>,
    npm_cache: Cache<Vec<LeakItem>>,
    feed_cache: Cache<Vec<WorldFeedItem>>,
    ghost_cache: Cache<Vec<GhostRepo>>,
    twitter_cache: Cache<Vec<VipItem>>,
    reddit_cache: Cache<Vec<VipItem>>,
    newsmap_cache: Cache<Vec<NewsMapItem>>,
    worldevents_cache: Cache<Vec<WorldEventItem>>,
    /// Cached anonymous Reddit OAuth token (token, acquired_at).
    /// Refreshed automatically every 55 min via get_reddit_token().
    reddit_token_cache: Arc<RwLock<Option<(String, Instant)>>>,
}

impl WebState {
    pub fn new(
        alerts: AlertBuf,
        findings: FindingsBuf,
        redis_store: Option<crate::redis_store::RedisStore>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .user_agent("repo-radar/1.0 (github.com/lemwaiping123-eng/repo-radar)")
            .timeout(Duration::from_secs(12))
            .build()
            .unwrap_or_default();
        let replicator = Arc::new(Replicator::new(http.clone()));
        Self {
            alerts,
            findings,
            http,
            redis_store,
            replicator,
            seen_repls: new_seen_replications(),
            vip_cache: Arc::new(RwLock::new(None)),
            social_cache: Arc::new(RwLock::new(None)),
            trend_cache: Arc::new(RwLock::new(None)),
            leak_cache: Arc::new(RwLock::new(None)),
            npm_cache: Arc::new(RwLock::new(None)),
            feed_cache: Arc::new(RwLock::new(None)),
            ghost_cache: Arc::new(RwLock::new(None)),
            twitter_cache: Arc::new(RwLock::new(None)),
            reddit_cache: Arc::new(RwLock::new(None)),
            newsmap_cache: Arc::new(RwLock::new(None)),
            worldevents_cache: Arc::new(RwLock::new(None)),
            reddit_token_cache: Arc::new(RwLock::new(None)),
        }
    }
}

/// Expose the findings buffer constructor publicly so main.rs can create one.
pub use crate::secret_scanner::new_findings_buf as new_findings_buf_alias;

// ─── API types ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VipItem {
    pub title: String,
    pub url: String,
    pub source: String,
    pub score: Option<u32>,
    pub by: Option<String>,
    pub vip_match: Option<String>,
    pub time: Option<i64>,
}
/// A unified world-feed item with priority tier classification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldFeedItem {
    pub title: String,
    pub url: String,
    pub source: String,
    pub score: Option<u32>,
    pub by: Option<String>,
    pub time: Option<i64>,
    /// "critical" = government/policy/breaking; "normal" = general tech/social
    pub tier: String,
    pub tags: Vec<String>,
}
/// A geolocated, tech-filtered news item for the News Map view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsMapItem {
    pub title: String,
    pub url: String,
    pub source: String,
    pub lat: f64,
    pub lng: f64,
    pub country: String,
    pub tier: String,
    pub time: Option<i64>,
    pub score: Option<u32>,
}

/// A single geolocated event for the world map — news or GitHub trending.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldEventItem {
    pub title: String,
    pub url: String,
    pub source: String,
    pub lat: f64,
    pub lng: f64,
    pub country: String,
    pub tier: String,
    /// "news" | "github"
    pub kind: String,
    pub time: Option<i64>,
    pub score: Option<u32>,
}

/// One item returned by the signal-fusion engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FusionItem {
    pub title: String,
    pub url: String,
    pub source: String,
    pub raw_score: f64,
}

/// Aggregated multi-source fusion result for a search topic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FusionResult {
    pub topic: String,
    pub hn_hits: u32,
    pub github_hits: u32,
    pub reddit_hits: u32,
    pub fused_score: f64,
    pub confidence: f64,
    pub items: Vec<FusionItem>,
}

/// One platform result from the social OSINT hunt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocialHit {
    pub platform: String,
    pub url: String,
    pub found: bool,
    pub icon: String,
}

/// A GitHub repo with many stars but very few commits — classic leak/dump profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GhostRepo {
    pub id: u64,
    pub full_name: String,
    pub url: String,
    pub description: String,
    pub stars: u32,
    pub forks: u32,
    /// Confirmed commit count (≤ 5 to qualify as ghost)
    pub commits: u32,
    pub created_at: String,
    pub pushed_at: String,
    pub language: Option<String>,
    pub avatar_url: String,
    pub owner: String,
    pub topics: Vec<String>,
    pub size_kb: u32,
}
/// A known or discovered leaked source code repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeakItem {
    pub id: String,
    pub name: String,
    pub description: String,
    pub leaked_at: String,
    pub discoverer: String,
    pub discoverer_url: String,
    pub root_cause: String,
    pub repo_url: String,
    pub clone_cmd: String,
    pub npm_pkg: Option<String>,
    pub mirrors: Vec<String>,
    pub tags: Vec<String>,
    pub language: Option<String>,
    pub confirmed: bool,
    pub severity: String,
}
// ─── Route handlers ───────────────────────────────────────────────────────────

async fn serve_dashboard() -> Html<&'static str> {
    Html(include_str!("../assets/index.html"))
}

async fn serve_worldmap() -> Html<&'static str> {
    Html(include_str!("../assets/worldmap.html"))
}

async fn api_alerts(State(s): State<Arc<WebState>>) -> Json<Vec<Alert>> {
    // Serve alerts from the last 3 days, sorted newest-first.
    let cutoff = Utc::now() - CDuration::days(3);
    let g = s.alerts.read().await;
    let mut alerts: Vec<Alert> = g
        .iter()
        .filter(|a| a.detected_at > cutoff)
        .cloned()
        .collect();
    // Sort: critical priority first, then by time desc
    alerts.sort_by(|a, b| {
        let ap = (a.priority == crate::detector::AlertPriority::Critical) as u8;
        let bp = (b.priority == crate::detector::AlertPriority::Critical) as u8;
        bp.cmp(&ap).then_with(|| b.detected_at.cmp(&a.detected_at))
    });
    Json(alerts)
}

async fn api_secrets(State(s): State<Arc<WebState>>) -> Json<Vec<serde_json::Value>> {
    // Rust in-memory findings
    let rust_findings: Vec<serde_json::Value> = {
        let g = s.findings.read().await;
        g.iter()
            .filter_map(|f| serde_json::to_value(f).ok())
            .collect()
    };

    // Python scanner findings from Redis repo-radar:secrets
    let mut redis_findings: Vec<serde_json::Value> = Vec::new();
    if let Some(store) = &s.redis_store {
        if let Ok(items) = store.get_raw_list("repo-radar:secrets", 500).await {
            for item in items {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&item) {
                    redis_findings.push(v);
                }
            }
        }
    }

    // Merge both sources, dedup by id field
    let mut seen_ids = std::collections::HashSet::new();
    let mut merged: Vec<serde_json::Value> =
        Vec::with_capacity(rust_findings.len() + redis_findings.len());
    for f in rust_findings.into_iter().chain(redis_findings) {
        let id = f
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if id.is_empty() || seen_ids.insert(id) {
            merged.push(f);
        }
    }
    Json(merged)
}

async fn api_vip(State(s): State<Arc<WebState>>) -> Json<Vec<VipItem>> {
    const TTL: Duration = Duration::from_secs(90);
    {
        let c = s.vip_cache.read().await;
        if let Some((at, ref data)) = *c {
            if at.elapsed() < TTL {
                return Json(data.clone());
            }
        }
    }
    let mut items = fetch_vip_feed(&s.http).await.unwrap_or_default();
    // Drop items older than 3 days (HN `time` is Unix timestamp).
    let cutoff_unix = (Utc::now() - CDuration::days(3)).timestamp();
    items.retain(|i| i.time.is_none_or(|t| t > cutoff_unix));
    *s.vip_cache.write().await = Some((Instant::now(), items.clone()));
    Json(items)
}

async fn api_trending(State(s): State<Arc<WebState>>) -> Json<serde_json::Value> {
    const TTL: Duration = Duration::from_secs(120);
    {
        let c = s.trend_cache.read().await;
        if let Some((at, ref data)) = *c {
            if at.elapsed() < TTL {
                return Json(data.clone());
            }
        }
    }
    let data = fetch_github_trending(&s.http)
        .await
        .unwrap_or(serde_json::json!([]));
    *s.trend_cache.write().await = Some((Instant::now(), data.clone()));
    Json(data)
}

async fn api_social(State(s): State<Arc<WebState>>) -> Json<Vec<VipItem>> {
    const TTL: Duration = Duration::from_secs(60);
    {
        let c = s.social_cache.read().await;
        if let Some((at, ref data)) = *c {
            if at.elapsed() < TTL {
                return Json(data.clone());
            }
        }
    }
    let mut items = fetch_social_feed(&s.http).await.unwrap_or_default();
    let cutoff_unix = (Utc::now() - CDuration::days(3)).timestamp();
    items.retain(|i| i.time.is_none_or(|t| t > cutoff_unix));
    *s.social_cache.write().await = Some((Instant::now(), items.clone()));
    Json(items)
}

async fn api_leaks(State(s): State<Arc<WebState>>) -> Json<Vec<LeakItem>> {
    const TTL: Duration = Duration::from_secs(600);
    {
        let c = s.leak_cache.read().await;
        if let Some((at, ref data)) = *c {
            if at.elapsed() < TTL {
                return Json(data.clone());
            }
        }
    }
    let mut items = known_leaks();
    if let Ok(found) = search_leaked_repos(&s.http).await {
        let existing: std::collections::HashSet<String> =
            items.iter().map(|i| i.repo_url.clone()).collect();
        for item in found {
            if !existing.contains(&item.repo_url) {
                items.push(item);
            }
        }
    }
    // Merge npm source-map monitor results (separate 15-min cache)
    {
        const NPM_TTL: Duration = Duration::from_secs(900);
        let cached = {
            let c = s.npm_cache.read().await;
            c.as_ref().and_then(|(at, ref data)| {
                if at.elapsed() < NPM_TTL {
                    Some(data.clone())
                } else {
                    None
                }
            })
        };
        let npm_items = match cached {
            Some(v) => v,
            None => {
                let v = check_npm_source_leaks(&s.http).await.unwrap_or_default();
                *s.npm_cache.write().await = Some((Instant::now(), v.clone()));
                v
            }
        };
        let existing_ids: std::collections::HashSet<String> =
            items.iter().map(|i| i.id.clone()).collect();
        for item in npm_items {
            if !existing_ids.contains(&item.id) {
                items.push(item);
            }
        }
    }
    // ── Auto-replication: trigger for any new confirmed/critical leak ────────
    for item in items
        .iter()
        .filter(|i| i.confirmed || i.severity == "critical")
    {
        let already_seen = s.seen_repls.read().await.contains(&item.id);
        if !already_seen {
            s.seen_repls.write().await.insert(item.id.clone());
            let repl = s.replicator.clone();
            let owned = item.clone();
            tokio::spawn(async move { repl.replicate(owned).await });
        }
    }

    *s.leak_cache.write().await = Some((Instant::now(), items.clone()));
    Json(items)
}

// ─── Data helpers ─────────────────────────────────────────────────────────────

async fn fetch_vip_feed(http: &reqwest::Client) -> Result<Vec<VipItem>> {
    let ids: Vec<u64> = http
        .get("https://hacker-news.firebaseio.com/v0/topstories.json")
        .send()
        .await?
        .json()
        .await?;

    let mut items: Vec<VipItem> = Vec::new();

    // Fetch first 200 IDs in parallel, 20 at a time.
    for chunk in ids.iter().take(200).collect::<Vec<_>>().chunks(20) {
        let futs: Vec<_> = chunk
            .iter()
            .map(|&&id| {
                let url = format!("https://hacker-news.firebaseio.com/v0/item/{id}.json");
                let h = http.clone();
                async move {
                    let Ok(resp) = h.get(&url).send().await else {
                        return None;
                    };
                    resp.json::<serde_json::Value>().await.ok()
                }
            })
            .collect();

        let results = futures::future::join_all(futs).await;

        for v in results.into_iter().flatten() {
            let title = v["title"].as_str().unwrap_or("").to_string();
            let story_url = v["url"].as_str().unwrap_or("").to_string();
            let combined = format!("{title} {story_url}").to_lowercase();

            let vip_match = VIP_TERMS
                .iter()
                .find(|&&term| combined.contains(term))
                .map(|s| s.to_string());
            let is_ai = AI_KEYWORDS.iter().any(|&kw| combined.contains(kw));

            if vip_match.is_some() || is_ai {
                let id_val = v["id"].as_u64().unwrap_or(0);
                let fallback = format!("https://news.ycombinator.com/item?id={id_val}");
                items.push(VipItem {
                    title,
                    url: if story_url.is_empty() {
                        fallback
                    } else {
                        story_url
                    },
                    source: "Hacker News".into(),
                    score: v["score"].as_u64().map(|s| s as u32),
                    by: v["by"].as_str().map(Into::into),
                    vip_match,
                    time: v["time"].as_i64(),
                });
            }
        }

        if items.len() >= 30 {
            break;
        }
    }

    // Reddit public JSON API blocked (403). Replaced with HN Algolia search.
    // ── HN Algolia: targeted VIP/AI keyword search ────────────────────────────
    if let Ok(resp) = http
        .get("https://hn.algolia.com/api/v1/search")
        .query(&[
            (
                "query",
                "AI claude anthropic openai github security leak breach",
            ),
            ("tags", "story"),
            ("hitsPerPage", "40"),
        ])
        .send()
        .await
    {
        if let Ok(data) = resp.json::<serde_json::Value>().await {
            if let Some(hits) = data["hits"].as_array() {
                for hit in hits {
                    let title = hit["title"].as_str().unwrap_or("").to_string();
                    let story_url = hit["url"].as_str().unwrap_or("").to_string();
                    let hn_id = hit["objectID"].as_str().unwrap_or("0");
                    let fallback = format!("https://news.ycombinator.com/item?id={hn_id}");
                    let combined = format!("{title} {story_url}").to_lowercase();
                    let vip_match = VIP_TERMS
                        .iter()
                        .find(|&&t| combined.contains(t))
                        .map(|s| s.to_string());
                    let is_ai = AI_KEYWORDS.iter().any(|&kw| combined.contains(kw));
                    if vip_match.is_some() || is_ai {
                        items.push(VipItem {
                            title,
                            url: if story_url.is_empty() {
                                fallback
                            } else {
                                story_url
                            },
                            source: "HN Algolia".into(),
                            score: hit["points"].as_u64().map(|v| v as u32),
                            by: hit["author"].as_str().map(Into::into),
                            vip_match,
                            time: hit["created_at_i"].as_i64(),
                        });
                    }
                }
            }
        }
    }

    // ── Dev.to: free API, no auth needed ─────────────────────────────────────
    for tag in &["ai", "machinelearning", "llm", "artificialintelligence"] {
        let url = format!("https://dev.to/api/articles?tag={tag}&per_page=15&state=rising");
        if let Ok(resp) = http.get(&url).send().await {
            if let Ok(articles) = resp.json::<Vec<serde_json::Value>>().await {
                for art in articles.iter().take(8) {
                    let title = art["title"].as_str().unwrap_or("").to_string();
                    let art_url = art["url"].as_str().unwrap_or("").to_string();
                    let combined = format!("{title} {art_url}").to_lowercase();
                    let vip_match = VIP_TERMS
                        .iter()
                        .find(|&&t| combined.contains(t))
                        .map(|s| s.to_string());
                    let is_ai = AI_KEYWORDS.iter().any(|&kw| combined.contains(kw));
                    if vip_match.is_some() || is_ai {
                        let ts = art["published_at"]
                            .as_str()
                            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                            .map(|dt| dt.timestamp());
                        items.push(VipItem {
                            title,
                            url: art_url,
                            source: "Dev.to".into(),
                            score: art["positive_reactions_count"].as_u64().map(|v| v as u32),
                            by: art["user"]["username"].as_str().map(Into::into),
                            vip_match,
                            time: ts,
                        });
                    }
                }
            }
        }
    }

    // VIP matches first, then by time desc (most recent first), then score.
    items.sort_by(|a, b| {
        let av = a.vip_match.is_some() as u8;
        let bv = b.vip_match.is_some() as u8;
        bv.cmp(&av)
            .then_with(|| b.time.unwrap_or(0).cmp(&a.time.unwrap_or(0)))
            .then_with(|| b.score.cmp(&a.score))
    });
    items.truncate(60);
    Ok(items)
}

/// Broader social feed: all Reddit + Dev.to + HN items, no AI keyword filter.
/// Used by `/api/social` to show a raw pulse of what's being talked about.
async fn fetch_social_feed(http: &reqwest::Client) -> Result<Vec<VipItem>> {
    let mut items: Vec<VipItem> = Vec::new();

    // Reddit public JSON API blocked (403). Replaced with HN Algolia + Lobsters.
    // ── HN Algolia: general recent stories ───────────────────────────────────
    for query in &[
        "programming security AI",
        "devops tech news",
        "open source release",
    ] {
        if let Ok(resp) = http
            .get("https://hn.algolia.com/api/v1/search_by_date")
            .query(&[("query", *query), ("tags", "story"), ("hitsPerPage", "20")])
            .send()
            .await
        {
            if let Ok(data) = resp.json::<serde_json::Value>().await {
                if let Some(hits) = data["hits"].as_array() {
                    for hit in hits {
                        let title = hit["title"].as_str().unwrap_or("").to_string();
                        if title.is_empty() {
                            continue;
                        }
                        let story_url = hit["url"].as_str().unwrap_or("").to_string();
                        let hn_id = hit["objectID"].as_str().unwrap_or("0");
                        let fallback = format!("https://news.ycombinator.com/item?id={hn_id}");
                        let combined = format!("{title} {story_url}").to_lowercase();
                        let vip_match = VIP_TERMS
                            .iter()
                            .find(|&&t| combined.contains(t))
                            .map(|s| s.to_string());
                        items.push(VipItem {
                            title,
                            url: if story_url.is_empty() {
                                fallback
                            } else {
                                story_url
                            },
                            source: "HN Algolia".into(),
                            score: hit["points"].as_u64().map(|v| v as u32),
                            by: hit["author"].as_str().map(Into::into),
                            vip_match,
                            time: hit["created_at_i"].as_i64(),
                        });
                    }
                }
            }
        }
    }
    // ── Lobsters: open tech community (free JSON, no auth) ───────────────────
    if let Ok(resp) = http.get("https://lobste.rs/hottest.json").send().await {
        if let Ok(stories) = resp.json::<Vec<serde_json::Value>>().await {
            for s in stories.iter().take(25) {
                let title = s["title"].as_str().unwrap_or("").to_string();
                let story_url = s["url"].as_str().unwrap_or("").to_string();
                if title.is_empty() || story_url.is_empty() {
                    continue;
                }
                let combined = format!("{title} {story_url}").to_lowercase();
                let vip_match = VIP_TERMS
                    .iter()
                    .find(|&&t| combined.contains(t))
                    .map(|s| s.to_string());
                items.push(VipItem {
                    title,
                    url: story_url,
                    source: "Lobsters".into(),
                    score: s["score"].as_u64().map(|v| v as u32),
                    by: s["submitter_user"].as_str().map(Into::into),
                    vip_match,
                    time: s["created_at"]
                        .as_str()
                        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                        .map(|dt| dt.timestamp()),
                });
            }
        }
    }

    // Dev.to — top articles, no filter
    for tag in &["ai", "devops", "webdev", "rust", "python"] {
        let url = format!("https://dev.to/api/articles?tag={tag}&per_page=8&state=rising");
        if let Ok(resp) = http.get(&url).send().await {
            if let Ok(articles) = resp.json::<Vec<serde_json::Value>>().await {
                for art in articles.iter().take(5) {
                    let title = art["title"].as_str().unwrap_or("").to_string();
                    let art_url = art["url"].as_str().unwrap_or("").to_string();
                    let combined = format!("{title} {art_url}").to_lowercase();
                    let vip_match = VIP_TERMS
                        .iter()
                        .find(|&&t| combined.contains(t))
                        .map(|s| s.to_string());
                    let ts = art["published_at"]
                        .as_str()
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                        .map(|dt| dt.timestamp());
                    items.push(VipItem {
                        title,
                        url: art_url,
                        source: "Dev.to".into(),
                        score: art["positive_reactions_count"].as_u64().map(|v| v as u32),
                        by: art["user"]["username"].as_str().map(Into::into),
                        vip_match,
                        time: ts,
                    });
                }
            }
        }
    }

    // Sort: VIP match first, then by time desc (most recent first), then score
    items.sort_by(|a, b| {
        let av = a.vip_match.is_some() as u8;
        let bv = b.vip_match.is_some() as u8;
        bv.cmp(&av)
            .then_with(|| b.time.unwrap_or(0).cmp(&a.time.unwrap_or(0)))
            .then_with(|| b.score.cmp(&a.score))
    });
    items.truncate(100);
    Ok(items)
}

// ─── Ghost Repos (high stars, low commits) ────────────────────────────────────

async fn api_ghost(State(s): State<Arc<WebState>>) -> Json<Vec<GhostRepo>> {
    const TTL: Duration = Duration::from_secs(300);
    {
        let c = s.ghost_cache.read().await;
        if let Some((at, ref data)) = *c {
            if at.elapsed() < TTL {
                return Json(data.clone());
            }
        }
    }
    let items = fetch_ghost_repos(&s.http).await.unwrap_or_default();
    *s.ghost_cache.write().await = Some((Instant::now(), items.clone()));
    Json(items)
}

async fn api_twitter(State(s): State<Arc<WebState>>) -> Json<Vec<VipItem>> {
    const TTL: Duration = Duration::from_secs(120);
    {
        let c = s.twitter_cache.read().await;
        if let Some((at, ref data)) = *c {
            if at.elapsed() < TTL {
                return Json(data.clone());
            }
        }
    }
    let items = fetch_twitter_viral().await.unwrap_or_default();
    *s.twitter_cache.write().await = Some((Instant::now(), items.clone()));
    Json(items)
}

async fn api_reddit(State(s): State<Arc<WebState>>) -> Json<Vec<VipItem>> {
    const TTL: Duration = Duration::from_secs(90);
    {
        let c = s.reddit_cache.read().await;
        if let Some((at, ref data)) = *c {
            if at.elapsed() < TTL {
                return Json(data.clone());
            }
        }
    }
    let items = fetch_reddit_viral(&s.http).await.unwrap_or_default();
    *s.reddit_cache.write().await = Some((Instant::now(), items.clone()));
    Json(items)
}

/// Search GitHub for repos that look like source dumps:
/// many stars, very few commits (≤ 5).  Sort by stars desc (biggest first).
async fn fetch_ghost_repos(http: &reqwest::Client) -> Result<Vec<GhostRepo>> {
    // Two complementary searches:
    // 1. All-time high-star original repos (classic viral dumps / famous leaks)
    // 2. Recent mid-star original repos created in the last year (fresh dumps)
    let year_ago = (Utc::now() - CDuration::days(365))
        .format("%Y-%m-%d")
        .to_string();

    let search_queries: Vec<String> = vec![
        "stars:>500 fork:false".to_string(),
        format!("stars:>200 fork:false created:>{year_ago}"),
    ];

    let mut all_items: Vec<serde_json::Value> = Vec::new();
    for q in &search_queries {
        if let Ok(resp) = http
            .get("https://api.github.com/search/repositories")
            .query(&[
                ("q", q.as_str()),
                ("sort", "stars"),
                ("order", "desc"),
                ("per_page", "30"),
            ])
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
        {
            if resp.status().is_success() {
                if let Ok(data) = resp.json::<serde_json::Value>().await {
                    if let Some(arr) = data["items"].as_array() {
                        all_items.extend_from_slice(arr);
                    }
                }
            }
        }
        // Brief pause between searches to stay within the 10-searches/min limit
        tokio::time::sleep(Duration::from_millis(400)).await;
    }

    // Deduplicate by repo id across both queries
    let mut seen_ids = std::collections::HashSet::new();
    all_items.retain(|r| seen_ids.insert(r["id"].as_u64().unwrap_or(0)));

    let items = all_items;

    let mut results = Vec::new();
    for repo in &items {
        let stars = repo["stargazers_count"].as_u64().unwrap_or(0) as u32;
        let forks = repo["forks_count"].as_u64().unwrap_or(0) as u32;
        let full_name = match repo["full_name"].as_str() {
            Some(n) => n.to_string(),
            None => continue,
        };

        // Ghost heuristic: low forks relative to stars (suspicious dump profile).
        // stars/6 threshold is more permissive than the old stars/15, catching
        // dumps that have accumulated a moderate number of archival forks.
        let ghost_ratio = forks < (stars / 6).max(2) || (stars >= 1000 && forks < 50);
        if !ghost_ratio {
            continue;
        }

        // Try to verify commit count (≤ 5 = one-shot dump).
        // If rate-limited or errored, still include the repo so unauthenticated
        // users see results — commits will show as 0 with a "?" marker.
        let commit_count = match fetch_commit_count(http, &full_name).await {
            Ok(n) => {
                if n > 5 {
                    continue; // too many commits, not a ghost
                }
                n
            }
            Err(_) => 0, // rate-limited or error — include with 0 commits shown
        };

        let owner = repo["owner"]["login"].as_str().unwrap_or("").to_string();
        let created_at = repo["created_at"].as_str().unwrap_or("").to_string();
        results.push(GhostRepo {
            id: repo["id"].as_u64().unwrap_or(0),
            full_name,
            url: repo["html_url"].as_str().unwrap_or("").to_string(),
            description: repo["description"].as_str().unwrap_or("").to_string(),
            stars,
            forks,
            commits: commit_count,
            created_at,
            pushed_at: repo["pushed_at"].as_str().unwrap_or("").to_string(),
            language: repo["language"].as_str().map(String::from),
            avatar_url: format!("https://avatars.githubusercontent.com/{owner}"),
            owner,
            topics: repo["topics"]
                .as_array()
                .map(|t| {
                    t.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
            size_kb: repo["size"].as_u64().unwrap_or(0) as u32,
        });
    }

    // Most-starred ghost repos first; break ties by newest creation
    results.sort_by(|a, b| {
        b.stars
            .cmp(&a.stars)
            .then_with(|| b.created_at.cmp(&a.created_at))
    });
    Ok(results)
}

/// Fetch the number of commits for a repo (up to 6, to stay in rate-limit budget).
async fn fetch_commit_count(http: &reqwest::Client, full_name: &str) -> Result<u32> {
    let resp = http
        .get(format!("https://api.github.com/repos/{full_name}/commits"))
        .query(&[("per_page", "6")])
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;
    if !resp.status().is_success() {
        anyhow::bail!("Commits API {} for {}", resp.status(), full_name);
    }
    let commits: serde_json::Value = resp.json().await?;
    Ok(commits.as_array().map(|a| a.len() as u32).unwrap_or(0))
}

/// Twitter / X viral feed — reads the twikit_cache.json populated by the
/// companion `twikit_feed.py` Python sidecar.  Returns items sorted by
/// engagement (retweets × 5 + likes + replies × 2 + quotes × 3).
/// If the cache file does not exist yet, returns an empty list gracefully.
async fn fetch_twitter_viral() -> Result<Vec<VipItem>> {
    use crate::sources::twitter::TwitterSource;
    let tw = TwitterSource::new("");
    let tweets = tw.search_tech_trending(0).await.unwrap_or_default();
    let mut items: Vec<VipItem> = tweets
        .into_iter()
        .map(|t| {
            let eng = t.public_metrics.engagement();
            let eng_label = if eng >= 1000 {
                format!("🔥 {:.1}k eng", eng as f64 / 1000.0)
            } else {
                format!("🔥 {} eng", eng)
            };
            let user = t.username.as_deref().unwrap_or("unknown").to_string();
            VipItem {
                title: t.text,
                url: format!("https://twitter.com/{user}/status/{}", t.id),
                source: format!("@{user}"),
                score: Some(t.public_metrics.like_count as u32),
                by: Some(user),
                vip_match: Some(eng_label),
                time: None,
            }
        })
        .collect();
    items.sort_by_key(|x| std::cmp::Reverse(x.score));
    items.truncate(50);
    Ok(items)
}

/// Reddit viral feed — fetches hot + rising posts from key tech subreddits,
/// deduplicates by URL, and sorts by upvote score descending so the most
/// engaging posts appear first.
/// Replaced Reddit (403-blocked) with HN Algolia + Lobsters for viral/trending posts.
async fn fetch_reddit_viral(http: &reqwest::Client) -> Result<Vec<VipItem>> {
    let mut items: Vec<VipItem> = Vec::new();

    // ── HN Algolia: top-voted recent stories ─────────────────────────────────
    for query in &[
        "Show HN",
        "Ask HN",
        "vulnerability discovered breach",
        "launches raises funding",
        "open source release",
    ] {
        if let Ok(resp) = http
            .get("https://hn.algolia.com/api/v1/search")
            .query(&[("query", *query), ("tags", "story"), ("hitsPerPage", "20")])
            .send()
            .await
        {
            if let Ok(data) = resp.json::<serde_json::Value>().await {
                if let Some(hits) = data["hits"].as_array() {
                    for hit in hits {
                        let title = hit["title"].as_str().unwrap_or("").to_string();
                        if title.is_empty() {
                            continue;
                        }
                        let score = hit["points"].as_u64().unwrap_or(0);
                        if score < 30 {
                            continue;
                        }
                        let story_url = hit["url"].as_str().unwrap_or("").to_string();
                        let hn_id = hit["objectID"].as_str().unwrap_or("0");
                        let comments = hit["num_comments"].as_u64().unwrap_or(0);
                        items.push(VipItem {
                            title,
                            url: if story_url.is_empty() {
                                format!("https://news.ycombinator.com/item?id={hn_id}")
                            } else {
                                story_url
                            },
                            source: "HN Viral".into(),
                            score: Some(score as u32),
                            by: hit["author"].as_str().map(Into::into),
                            vip_match: Some(format!("💬 {comments}")),
                            time: hit["created_at_i"].as_i64(),
                        });
                    }
                }
            }
        }
    }

    // ── Lobsters: hottest open-tech community posts ───────────────────────────
    if let Ok(resp) = http.get("https://lobste.rs/hottest.json").send().await {
        if let Ok(stories) = resp.json::<Vec<serde_json::Value>>().await {
            for s in stories.iter().take(40) {
                let title = s["title"].as_str().unwrap_or("").to_string();
                let story_url = s["url"].as_str().unwrap_or("").to_string();
                let score = s["score"].as_u64().unwrap_or(0);
                if title.is_empty() || score < 5 {
                    continue;
                }
                let comments = s["comment_count"].as_u64().unwrap_or(0);
                items.push(VipItem {
                    title,
                    url: story_url,
                    source: "Lobsters".into(),
                    score: Some(score as u32),
                    by: s["submitter_user"].as_str().map(Into::into),
                    vip_match: Some(format!("💬 {comments}")),
                    time: s["created_at"]
                        .as_str()
                        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                        .map(|dt| dt.timestamp()),
                });
            }
        }
    }

    let mut seen = std::collections::HashSet::new();
    items.retain(|it| seen.insert(it.url.clone()));
    items.sort_by_key(|x| std::cmp::Reverse(x.score));
    items.truncate(80);
    Ok(items)
}

async fn fetch_github_trending(http: &reqwest::Client) -> Result<serde_json::Value> {
    // Require an authenticated token — unauthenticated calls exhaust the 60 req/hr
    // limit almost immediately and return rate-limit error JSON instead of repos.
    let token = std::env::var("GITHUB_TOKEN").ok();
    if token.is_none() {
        return Ok(serde_json::json!([]));
    }
    let since = (Utc::now() - CDuration::days(30))
        .format("%Y-%m-%d")
        .to_string();
    let q = format!("created:>{since} stars:>10");

    let resp = http
        .get("https://api.github.com/search/repositories")
        .query(&[
            ("q", q.as_str()),
            ("sort", "stars"),
            ("order", "desc"),
            ("per_page", "15"),
        ])
        .header("Accept", "application/vnd.github+json")
        .bearer_auth(token.unwrap())
        .send()
        .await?;

    if !resp.status().is_success() {
        // Rate-limited or error — return empty rather than propagating failure.
        return Ok(serde_json::json!([]));
    }

    let data: serde_json::Value = resp.json().await?;
    Ok(data["items"].clone())
}

/// Hard-coded known confirmed leaks.
fn known_leaks() -> Vec<LeakItem> {
    vec![
        LeakItem {
            id: "claude-code-2026".into(),
            name: "Claude Code — Full TypeScript Source".into(),
            description: "Anthropic's Claude Code VS Code extension exposed via Bun-generated source maps bundled in the npm package. The .map files embed full original TypeScript source in their sourcesContent field. Revealed unreleased features: BUDDY (Tamagotchi), KAIROS (always-on assistant), ULTRAPLAN, Coordinator/Swarm multi-agent, and unreleased models Capybara / Opus 4.7 / Sonnet 4.8.".into(),
            leaked_at: "2026-03-31T16:23:00Z".into(),
            discoverer: "Chaofan Shou (@Fried_rice)".into(),
            discoverer_url: "https://x.com/Fried_rice/status/2038894956459290963".into(),
            root_cause: "Bun build tool generates *.map files by default.\n.npmignore did not exclude *.map files.\nPackage @anthropic-ai/claude-code published to npm with all source maps included.\nEach .map JSON contains a sourcesContent array with full TypeScript source.".into(),
            repo_url: "https://github.com/Kuberwastaken/claude-code".into(),
            clone_cmd: "git clone https://github.com/Kuberwastaken/claude-code".into(),
            npm_pkg: Some("@anthropic-ai/claude-code".into()),
            mirrors: vec![
                "https://github.com/Kuberwastaken/claude-code".into(),
            ],
            tags: vec![
                "Source Map".into(), "TypeScript".into(), "NPM".into(),
                "Anthropic".into(), "LLM".into(), "VS Code Extension".into(),
            ],
            language: Some("TypeScript".into()),
            confirmed: true,
            severity: "critical".into(),
        },
    ]
}

/// Search GitHub for recently-created repos that look like leaked AI source code.
async fn search_leaked_repos(http: &reqwest::Client) -> Result<Vec<LeakItem>> {
    let q = "leaked source-code AI LLM typescript stars:>5 created:>2026-01-01";
    let resp = http
        .get("https://api.github.com/search/repositories")
        .query(&[
            ("q", q),
            ("sort", "stars"),
            ("order", "desc"),
            ("per_page", "10"),
        ])
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;
    if !resp.status().is_success() {
        return Ok(vec![]);
    }
    let data: serde_json::Value = resp.json().await?;
    let arr = match data["items"].as_array() {
        Some(a) => a.clone(),
        None => return Ok(vec![]),
    };
    Ok(arr
        .into_iter()
        .filter_map(|r| {
            let full_name = r["full_name"].as_str()?.to_string();
            let desc = r["description"].as_str().unwrap_or("").to_string();
            let html_url = r["html_url"].as_str()?.to_string();
            let clone_url = r["clone_url"].as_str().unwrap_or("").to_string();
            let lang = r["language"].as_str().map(String::from);
            let stars = r["stargazers_count"].as_u64().unwrap_or(0);
            let combined = format!("{full_name} {desc}").to_lowercase();
            // Only keep repos that look genuinely leak-related
            if !combined.contains("leak")
                && !combined.contains("source-map")
                && !combined.contains("sourcemap")
            {
                return None;
            }
            let owner = r["owner"]["login"]
                .as_str()
                .unwrap_or("unknown")
                .to_string();
            let created = r["created_at"].as_str().unwrap_or("").to_string();
            let mut tags = vec!["GitHub".to_string()];
            if let Some(l) = &lang {
                tags.push(l.clone());
            }
            if combined.contains("leak") {
                tags.push("Leaked".into());
            }
            if combined.contains("source-map") || combined.contains("sourcemap") {
                tags.push("Source Map".into());
            }
            Some(LeakItem {
                id: full_name.clone(),
                name: full_name.clone(),
                description: desc,
                leaked_at: created,
                discoverer: owner.clone(),
                discoverer_url: format!("https://github.com/{owner}"),
                root_cause: "Discovered on GitHub — verify independently before trusting".into(),
                clone_cmd: if clone_url.is_empty() {
                    format!("git clone {html_url}")
                } else {
                    format!("git clone {clone_url}")
                },
                mirrors: vec![html_url.clone()],
                npm_pkg: None,
                tags,
                language: lang,
                confirmed: false,
                severity: if stars > 100 {
                    "high".into()
                } else {
                    "medium".into()
                },
                repo_url: html_url,
            })
        })
        .collect())
}

/// Query the npm registry for known AI CLI packages and flag bundles with
/// anomalously large unpackedSize or file counts — a signal that TypeScript
/// source maps (`sourcesContent`) may have been accidentally shipped.
async fn check_npm_source_leaks(http: &reqwest::Client) -> Result<Vec<LeakItem>> {
    // (package, publisher, suspicious_mb_threshold, suspicious_file_threshold)
    const PACKAGES: &[(&str, &str, u64, u64)] = &[
        ("@anthropic-ai/claude-code", "Anthropic", 25, 350),
        ("@openai/codex", "OpenAI", 25, 350),
        ("@google-labs/gemini-cli", "Google", 25, 350),
        ("@mistralai/mistral-code", "Mistral", 20, 300),
    ];

    let mut items = Vec::new();

    for &(pkg, company, mb_thresh, file_thresh) in PACKAGES {
        let url = format!("https://registry.npmjs.org/{pkg}");
        let Ok(resp) = http
            .get(&url)
            .header("Accept", "application/json")
            .send()
            .await
        else {
            continue;
        };
        if !resp.status().is_success() {
            continue;
        }
        let Ok(data) = resp.json::<serde_json::Value>().await else {
            continue;
        };
        let Some(latest) = data["dist-tags"]["latest"].as_str().map(str::to_string) else {
            continue;
        };

        let dist = &data["versions"][latest.as_str()]["dist"];
        let unpacked_mb = dist["unpackedSize"].as_u64().unwrap_or(0) / 1_048_576;
        let file_count = dist["fileCount"].as_u64().unwrap_or(0);

        if unpacked_mb < mb_thresh && file_count < file_thresh {
            continue;
        }

        let npm_url = format!("https://www.npmjs.com/package/{pkg}");
        // npm pack creates <name>-<version>.tgz without the leading '@' or '/'
        let slug = pkg.trim_start_matches('@').replace('/', "-");
        items.push(LeakItem {
            id: format!("npm-{slug}"),
            name: format!("{company} {pkg} — unusually large bundle ({unpacked_mb} MB)"),
            description: format!(
                "v{latest}: {unpacked_mb} MB unpacked / {file_count} files. \
                 AI CLI tools of this size may embed TypeScript source maps \
                 (sourcesContent field) that expose original source code. \
                 The 2026 Claude Code leak followed exactly this pattern."
            ),
            leaked_at: chrono::Utc::now().to_rfc3339(),
            discoverer: "repo-radar npm monitor".into(),
            discoverer_url: npm_url.clone(),
            root_cause: format!(
                "Package {pkg}@{latest} has {unpacked_mb} MB / {file_count} files. \
                 Run: npm pack {pkg} && tar -tzf {slug}-{latest}.tgz | grep '\\.map$'"
            ),
            repo_url: npm_url.clone(),
            clone_cmd: format!("npm pack {pkg} && tar -tzf {slug}-{latest}.tgz | grep '\\.map$'"),
            npm_pkg: Some(pkg.into()),
            mirrors: vec![
                npm_url,
                format!("https://registry.npmjs.org/{pkg}/{latest}"),
            ],
            tags: vec!["NPM".into(), "Source Map Check".into(), company.into()],
            language: Some("TypeScript".into()),
            confirmed: false,
            severity: if unpacked_mb > 80 || file_count > 700 {
                "critical".into()
            } else {
                "high".into()
            },
        });
    }

    Ok(items)
}

// ─── World Feed: dual-tier combined feed ─────────────────────────────────────

/// Keywords that elevate an item to "critical" tier (government/policy/security).
const CRITICAL_KEYWORDS: &[&str] = &[
    "government",
    "congress",
    "parliament",
    "senate",
    "legislation",
    "regulation",
    "executive order",
    "sanctions",
    "election",
    "arrested",
    "indicted",
    "warrant",
    "seized",
    "shutdown",
    "classified",
    "whistleblower",
    "breach",
    "ransomware",
    "zero-day",
    "0day",
    "backdoor",
    "surveillance",
    "nsa",
    "cia",
    "fbi",
    "dhs",
    "cisa",
    "antitrust",
    "ftc",
    "gdpr",
    "censored",
    "banned",
    "nuclear",
    "military",
    "coup",
    "leaked",
    "dmca",
    "takedown",
    "cyberattack",
    "data breach",
    "hack",
    "exploit",
];

fn classify_tier(text: &str) -> (&'static str, Vec<String>) {
    let lower = text.to_lowercase();
    let matched: Vec<String> = CRITICAL_KEYWORDS
        .iter()
        .filter(|&&kw| lower.contains(kw))
        .map(|s| s.to_string())
        .collect();
    if matched.is_empty() {
        ("normal", vec![])
    } else {
        ("critical", matched)
    }
}

async fn api_feed(State(s): State<Arc<WebState>>) -> Json<Vec<WorldFeedItem>> {
    const TTL: Duration = Duration::from_secs(90);
    {
        let c = s.feed_cache.read().await;
        if let Some((at, ref data)) = *c {
            if at.elapsed() < TTL {
                return Json(data.clone());
            }
        }
    }
    let mut items = fetch_world_feed(&s.http).await.unwrap_or_default();
    let cutoff_unix = (Utc::now() - CDuration::days(3)).timestamp();
    items.retain(|i| i.time.is_none_or(|t| t > cutoff_unix));
    // Sort: critical first, then by time desc
    items.sort_by(|a, b| {
        let atier = (a.tier == "critical") as u8;
        let btier = (b.tier == "critical") as u8;
        btier
            .cmp(&atier)
            .then_with(|| b.time.unwrap_or(0).cmp(&a.time.unwrap_or(0)))
    });
    items.truncate(200);
    *s.feed_cache.write().await = Some((Instant::now(), items.clone()));
    Json(items)
}

/// Aggregates HN, Reddit (broad), and RSS sources into a single classified feed.
async fn fetch_world_feed(http: &reqwest::Client) -> Result<Vec<WorldFeedItem>> {
    let mut items: Vec<WorldFeedItem> = Vec::new();

    // ── HN top stories (all, not keyword-filtered) ───────────────────────────
    let ids: Vec<u64> = match http
        .get("https://hacker-news.firebaseio.com/v0/topstories.json")
        .send()
        .await
    {
        Ok(resp) => resp.json::<Vec<u64>>().await.unwrap_or_default(),
        Err(_) => vec![],
    };

    for chunk in ids.iter().take(100).collect::<Vec<_>>().chunks(20) {
        let futs: Vec<_> = chunk
            .iter()
            .map(|&&id| {
                let url = format!("https://hacker-news.firebaseio.com/v0/item/{id}.json");
                let h = http.clone();
                async move {
                    let Ok(resp) = h.get(&url).send().await else {
                        return None;
                    };
                    resp.json::<serde_json::Value>().await.ok()
                }
            })
            .collect();

        let results = futures::future::join_all(futs).await;
        for v in results.into_iter().flatten() {
            let title = v["title"].as_str().unwrap_or("").to_string();
            if title.is_empty() {
                continue;
            }
            let story_url = v["url"].as_str().unwrap_or("").to_string();
            let id_val = v["id"].as_u64().unwrap_or(0);
            let combined = format!("{title} {story_url}");
            let (tier, tags) = classify_tier(&combined);
            items.push(WorldFeedItem {
                title,
                url: if story_url.is_empty() {
                    format!("https://news.ycombinator.com/item?id={id_val}")
                } else {
                    story_url
                },
                source: "Hacker News".into(),
                score: v["score"].as_u64().map(|s| s as u32),
                by: v["by"].as_str().map(Into::into),
                time: v["time"].as_i64(),
                tier: tier.into(),
                tags,
            });
        }
    }

    // Reddit public JSON API blocked (403). Replaced with RSS world news feeds.
    // ── RSS World News: BBC, Guardian, Reuters ───────────────────────────────
    let world_rss: &[(&str, &str)] = &[
        ("BBC World",       "https://feeds.bbci.co.uk/news/world/rss.xml"),
        ("BBC Technology",  "https://feeds.bbci.co.uk/news/technology/rss.xml"),
        ("Guardian World",  "https://www.theguardian.com/world/rss"),
        ("Reuters Tech",    "https://feeds.reuters.com/reuters/technologyNews"),
        ("HN Front",        "https://hnrss.org/frontpage?count=40"),
        ("Lobsters RSS",    "https://lobste.rs/rss"),
        ("Google Geopolitics", "https://news.google.com/rss/search?q=(sanctions+OR+conflict+OR+war+OR+election+OR+treaty)+(government+OR+military+OR+tech)&hl=en-US&gl=US&ceid=US:en"),
    ];
    // Fetch all RSS feeds in parallel (was sequential — huge latency improvement)
    let rss_src = std::sync::Arc::new(crate::sources::rss::RssSource::new());
    let rss_futs: Vec<_> = world_rss
        .iter()
        .map(|&(name, url)| {
            let rss = std::sync::Arc::clone(&rss_src);
            async move { rss.fetch_one(name, url).await.unwrap_or_default() }
        })
        .collect();
    let rss_results = futures::future::join_all(rss_futs).await;
    for feed_items in rss_results {
        for fi in feed_items {
            let combined = format!("{} {}", fi.title, fi.link);
            let (tier, tags) = classify_tier(&combined);
            items.push(WorldFeedItem {
                title: fi.title,
                url: fi.link,
                source: fi.feed_name,
                score: None,
                by: None,
                time: fi.published.map(|dt| dt.timestamp()),
                tier: tier.into(),
                tags,
            });
        }
    }

    Ok(items)
}

// ─── helper: TV/entertainment noise filter ────────────────────────────────────

/// Return `false` for TV / entertainment noise so the map stays tech-only.
fn is_tech_relevant(text: &str) -> bool {
    let lo = text.to_lowercase();
    const NOISE: &[&str] = &[
        "netflix",
        "hbo max",
        "disney+",
        "hulu",
        "peacock",
        "paramount+",
        "prime video",
        "apple tv+",
        " tv show",
        "television show",
        " new season",
        "season finale",
        " episode ",
        "box office",
        "movie review",
        "film review",
        "grammy award",
        "oscar award",
        "emmy award",
        "golden globe",
        "bafta",
        "kardashian",
        "music video",
        "album drop",
        "chart-topping",
        "nfl draft",
        "nba trade",
        "fifa world cup",
        "super bowl",
        "reality show",
        "talk show",
        "soap opera",
    ];
    !NOISE.iter().any(|kw| lo.contains(kw))
}

/// Map a news item to approximate lat/lng via a keyword lookup table.
fn geo_tag(title: &str, url: &str) -> Option<(f64, f64, &'static str)> {
    let hay = format!("{title} {url}").to_lowercase();
    const PLACES: &[(&str, f64, f64, &str)] = &[
        // US cities / tech hubs
        ("silicon valley", 37.4, -122.1, "USA"),
        ("san francisco", 37.7, -122.4, "USA"),
        ("new york", 40.7, -74.0, "USA"),
        ("seattle", 47.6, -122.3, "USA"),
        ("boston", 42.4, -71.1, "USA"),
        ("austin", 30.3, -97.7, "USA"),
        ("washington dc", 38.9, -77.0, "USA"),
        ("los angeles", 34.1, -118.2, "USA"),
        // US companies → SF / Seattle as proxy geo
        ("openai", 37.7, -122.4, "USA"),
        ("anthropic", 37.8, -122.4, "USA"),
        ("google", 37.4, -122.1, "USA"),
        ("microsoft", 47.6, -122.1, "USA"),
        ("apple", 37.3, -122.0, "USA"),
        ("meta ", 37.5, -122.2, "USA"),
        ("amazon", 47.6, -122.3, "USA"),
        ("nvidia", 37.4, -122.0, "USA"),
        ("spacex", 33.9, -118.4, "USA"),
        ("tesla", 37.4, -122.0, "USA"),
        // Generic US signals
        ("united states", 38.0, -97.0, "USA"),
        ("u.s.", 38.0, -97.0, "USA"),
        ("american", 38.0, -97.0, "USA"),
        ("congress", 38.9, -77.0, "USA"),
        (" nsa ", 39.1, -76.8, "USA"),
        (" cia ", 38.9, -77.1, "USA"),
        // UK
        ("london", 51.5, -0.1, "UK"),
        ("united kingdom", 51.5, -0.1, "UK"),
        ("britain", 51.5, -2.0, "UK"),
        ("deepmind", 51.5, -0.1, "UK"),
        ("arm holdings", 51.5, -0.1, "UK"),
        // Europe
        ("paris", 48.9, 2.3, "France"),
        ("berlin", 52.5, 13.4, "Germany"),
        ("amsterdam", 52.4, 4.9, "Netherlands"),
        ("stockholm", 59.3, 18.1, "Sweden"),
        ("brussels", 50.8, 4.4, "Belgium"),
        ("munich", 48.1, 11.6, "Germany"),
        ("german", 51.2, 10.5, "Germany"),
        ("european union", 50.8, 4.4, "EU"),
        ("europe", 50.0, 10.0, "Europe"),
        // China
        ("beijing", 39.9, 116.4, "China"),
        ("shanghai", 31.2, 121.5, "China"),
        ("alibaba", 30.3, 120.1, "China"),
        ("tencent", 22.5, 114.1, "China"),
        ("baidu", 39.9, 116.4, "China"),
        ("huawei", 22.5, 114.1, "China"),
        ("china", 35.9, 104.2, "China"),
        ("chinese", 35.9, 104.2, "China"),
        // Japan / Korea
        ("tokyo", 35.7, 139.7, "Japan"),
        ("japan", 36.2, 138.3, "Japan"),
        ("sony", 35.7, 139.7, "Japan"),
        ("softbank", 35.7, 139.7, "Japan"),
        ("seoul", 37.6, 127.0, "S.Korea"),
        ("samsung", 37.5, 127.0, "S.Korea"),
        ("korea", 36.0, 128.0, "S.Korea"),
        // India
        ("india", 20.6, 79.1, "India"),
        ("bangalore", 12.9, 77.6, "India"),
        ("mumbai", 19.1, 72.9, "India"),
        // Singapore / Taiwan / Australia
        ("singapore", 1.4, 103.8, "Singapore"),
        ("taiwan", 23.7, 121.0, "Taiwan"),
        ("tsmc", 24.8, 120.9, "Taiwan"),
        ("australia", -25.3, 133.8, "Australia"),
        ("sydney", -33.9, 151.2, "Australia"),
        // Middle East
        ("israel", 31.5, 34.8, "Israel"),
        ("tel aviv", 32.1, 34.8, "Israel"),
        ("saudi", 24.7, 46.7, "Saudi Arabia"),
        // Americas
        ("canada", 56.1, -106.3, "Canada"),
        ("toronto", 43.7, -79.4, "Canada"),
        ("brazil", -14.2, -51.9, "Brazil"),
        ("mexico", 23.6, -102.6, "Mexico"),
    ];
    for &(kw, lat, lng, country) in PLACES {
        if hay.contains(kw) {
            return Some((lat, lng, country));
        }
    }
    None
}

// ─── /api/newsmap ─────────────────────────────────────────────────────────────

async fn api_newsmap(State(s): State<Arc<WebState>>) -> Json<Vec<NewsMapItem>> {
    const TTL: Duration = Duration::from_secs(120);
    {
        let c = s.newsmap_cache.read().await;
        if let Some((at, ref data)) = *c {
            if at.elapsed() < TTL {
                return Json(data.clone());
            }
        }
    }
    let feed = fetch_world_feed(&s.http).await.unwrap_or_default();
    let items: Vec<NewsMapItem> = feed
        .into_iter()
        .filter(|item| is_tech_relevant(&item.title))
        .filter_map(|item| {
            geo_tag(&item.title, &item.url).map(|(lat, lng, country)| NewsMapItem {
                title: item.title,
                url: item.url,
                source: item.source,
                lat,
                lng,
                country: country.to_string(),
                tier: item.tier,
                time: item.time,
                score: item.score,
            })
        })
        .collect();
    *s.newsmap_cache.write().await = Some((Instant::now(), items.clone()));
    Json(items)
}

// ─── /api/worldevents ───────────────────────────────────────────────────────

async fn api_worldevents(State(s): State<Arc<WebState>>) -> Json<Vec<WorldEventItem>> {
    const TTL: Duration = Duration::from_secs(120);
    {
        let c = s.worldevents_cache.read().await;
        if let Some((at, ref data)) = *c {
            if at.elapsed() < TTL {
                return Json(data.clone());
            }
        }
    }

    let mut events: Vec<WorldEventItem> = Vec::new();

    // Run world-feed and GitHub fetch concurrently
    let (feed_result, gh_result) =
        tokio::join!(fetch_world_feed(&s.http), fetch_github_trending(&s.http));

    // ── News items from world feed ──────────────────────────────────────────
    let feed = feed_result.unwrap_or_default();
    for item in feed {
        if let Some((lat, lng, country)) = geo_tag(&item.title, &item.url) {
            events.push(WorldEventItem {
                title: item.title,
                url: item.url,
                source: item.source,
                lat,
                lng,
                country: country.to_string(),
                tier: item.tier,
                kind: "news".to_string(),
                time: item.time,
                score: item.score,
            });
        }
    }

    // ── GitHub trending repos ───────────────────────────────────────────────
    if let Ok(gh_data) = gh_result {
        if let Some(repos) = gh_data.as_array() {
            for repo in repos {
                let name = repo["name"].as_str().unwrap_or("").to_string();
                let full_name = repo["full_name"].as_str().unwrap_or("").to_string();
                let desc = repo["description"].as_str().unwrap_or("").to_string();
                let url = repo["html_url"].as_str().unwrap_or("").to_string();
                let stars = repo["stargazers_count"].as_u64().map(|v| v as u32);
                // Try geo-tagging from repo description + full_name
                let geo_title = format!("{full_name} {desc}");
                let (lat, lng, country) =
                    geo_tag(&geo_title, &url).unwrap_or((37.09, -95.71, "USA"));
                let title = if desc.is_empty() {
                    full_name.clone()
                } else {
                    format!("{name}: {desc}")
                };
                events.push(WorldEventItem {
                    title,
                    url,
                    source: "GitHub Trending".to_string(),
                    lat,
                    lng,
                    country: country.to_string(),
                    tier: "normal".to_string(),
                    kind: "github".to_string(),
                    time: None,
                    score: stars,
                });
            }
        }
    }

    *s.worldevents_cache.write().await = Some((Instant::now(), events.clone()));
    Json(events)
}

// ─── /api/fusion?topic= ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct FusionQuery {
    topic: Option<String>,
}

async fn api_fusion(
    State(s): State<Arc<WebState>>,
    Query(q): Query<FusionQuery>,
) -> Json<FusionResult> {
    let topic = q
        .topic
        .unwrap_or_else(|| "AI".to_string())
        .chars()
        .take(80)
        .collect::<String>();

    let (hn_res, gh_res, rd_res) = tokio::join!(
        fetch_hn_fusion(&s.http, &topic),
        fetch_github_fusion(&s.http, &topic),
        fetch_reddit_fusion(&s, &topic),
    );

    let mut all_items: Vec<FusionItem> = Vec::new();
    let mut hn_hits = 0u32;
    let mut github_hits = 0u32;
    let mut reddit_hits = 0u32;

    if let Ok(items) = hn_res {
        hn_hits = items.len() as u32;
        all_items.extend(items);
    }
    if let Ok(items) = gh_res {
        github_hits = items.len() as u32;
        all_items.extend(items);
    }
    if let Ok(items) = rd_res {
        reddit_hits = items.len() as u32;
        all_items.extend(items);
    }

    all_items.sort_by(|a, b| {
        b.raw_score
            .partial_cmp(&a.raw_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    all_items.truncate(20);

    let total = (hn_hits + github_hits + reddit_hits) as f64;
    let fused_score = if total == 0.0 {
        0.0
    } else {
        (hn_hits as f64 * 3.0 + github_hits as f64 * 1.5 + reddit_hits as f64 * 1.0) / (total * 3.0)
            * 100.0
    };
    let sources_active = [hn_hits, github_hits, reddit_hits]
        .iter()
        .filter(|&&x| x > 0)
        .count() as f64;
    let confidence = (sources_active / 3.0) * 100.0;

    Json(FusionResult {
        topic,
        hn_hits,
        github_hits,
        reddit_hits,
        fused_score,
        confidence,
        items: all_items,
    })
}

async fn fetch_hn_fusion(http: &reqwest::Client, topic: &str) -> Result<Vec<FusionItem>> {
    let day_ago = Utc::now().timestamp() - 86400 * 7;
    let resp = http
        .get("https://hn.algolia.com/api/v1/search")
        .query(&[
            ("query", topic),
            ("tags", "story"),
            ("hitsPerPage", "15"),
            ("numericFilters", &format!("created_at_i>{day_ago}")),
        ])
        .send()
        .await?;
    let data: serde_json::Value = resp.json().await?;
    let items = data["hits"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|h| {
            let title = h["title"].as_str()?.to_string();
            let url = h["url"].as_str().unwrap_or("").to_string();
            let hn_id = h["objectID"].as_str().unwrap_or("").to_string();
            let score = h["points"].as_f64().unwrap_or(0.0);
            Some(FusionItem {
                title,
                url: if url.is_empty() {
                    format!("https://news.ycombinator.com/item?id={hn_id}")
                } else {
                    url
                },
                source: "HackerNews".into(),
                raw_score: score * 3.0,
            })
        })
        .collect();
    Ok(items)
}

async fn fetch_github_fusion(http: &reqwest::Client, topic: &str) -> Result<Vec<FusionItem>> {
    let token = std::env::var("GITHUB_TOKEN").ok();
    if token.is_none() {
        return Ok(vec![]);
    }
    let resp = http
        .get("https://api.github.com/search/repositories")
        .query(&[("q", topic), ("sort", "stars"), ("per_page", "10")])
        .header("User-Agent", "repo-radar/1.0")
        .bearer_auth(token.unwrap())
        .send()
        .await?;
    if !resp.status().is_success() {
        return Ok(vec![]);
    }
    let data: serde_json::Value = resp.json().await?;
    let items = data["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|r| {
            let name = r["full_name"].as_str()?.to_string();
            let url = r["html_url"].as_str()?.to_string();
            let stars = r["stargazers_count"].as_f64().unwrap_or(0.0);
            let desc = r["description"].as_str().unwrap_or("").to_string();
            Some(FusionItem {
                title: format!("{name} — {desc}").chars().take(120).collect(),
                url,
                source: "GitHub".into(),
                raw_score: stars.ln_1p() * 1.5,
            })
        })
        .collect();
    Ok(items)
}

/// Acquire and cache a Reddit anonymous OAuth token (redlib approach).
/// Requires REDDIT_CLIENT_ID env var to be set to your Reddit app client_id.
/// Token is cached for 55 min; auto-refreshed on expiry.
/// Returns None when REDDIT_CLIENT_ID is unset or acquisition fails.
async fn get_reddit_token(state: &WebState) -> Option<String> {
    const TTL: Duration = Duration::from_secs(55 * 60);
    {
        let guard = state.reddit_token_cache.read().await;
        if let Some((ref token, acquired_at)) = *guard {
            if acquired_at.elapsed() < TTL {
                return Some(token.clone());
            }
        }
    }
    let client_id = std::env::var("REDDIT_CLIENT_ID").ok()?;
    // Stable device_id per process start (not truly random — enough for Reddit)
    let device_id = format!(
        "{:016x}{:08x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::process::id()
    );
    let resp = state
        .http
        .post("https://www.reddit.com/api/v1/access_token")
        .basic_auth(&client_id, Some(""))
        .header(
            "User-Agent",
            "android:com.reddit.frontpage:v2023.21.0 (Linux; Android 13; SM-G991B)",
        )
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
        tracing::warn!(
            status = %resp.status(),
            "Reddit OAuth token acquisition failed"
        );
        return None;
    }
    let data: serde_json::Value = resp.json().await.ok()?;
    let token = data["access_token"].as_str()?.to_string();
    tracing::info!("Reddit OAuth token acquired (valid ~1 hr)");
    *state.reddit_token_cache.write().await = Some((token.clone(), Instant::now()));
    Some(token)
}

/// Reddit search via anonymous OAuth token (redlib approach) or direct API.
/// If REDDIT_CLIENT_ID is set, uses oauth.reddit.com with Bearer auth.
/// Falls back to www.reddit.com with browser UA; returns empty on 403.
async fn fetch_reddit_fusion(state: &WebState, topic: &str) -> Result<Vec<FusionItem>> {
    let token = get_reddit_token(state).await;
    let (api_url, ua) = if token.is_some() {
        (
            "https://oauth.reddit.com/search.json",
            "android:com.reddit.frontpage:v2023.21.0 (Linux; Android 13; SM-G991B)",
        )
    } else {
        (
            "https://www.reddit.com/search.json",
            "Mozilla/5.0 (X11; Linux x86_64; rv:124.0) Gecko/20100101 Firefox/124.0",
        )
    };
    let mut builder = state
        .http
        .get(api_url)
        .query(&[
            ("q", topic),
            ("sort", "top"),
            ("t", "week"),
            ("limit", "10"),
        ])
        .header("User-Agent", ua);
    if let Some(ref t) = token {
        builder = builder.bearer_auth(t);
    }
    let resp = builder.send().await?;
    if !resp.status().is_success() {
        tracing::debug!(status = %resp.status(), topic, "Reddit fusion search skipped");
        return Ok(vec![]);
    }
    let data: serde_json::Value = resp.json().await?;
    let items = data["data"]["children"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| {
            let d = p["data"].clone();
            let title = d["title"].as_str()?.to_string();
            let score = d["score"].as_f64().unwrap_or(0.0);
            let permalink = d["permalink"].as_str().unwrap_or("").to_string();
            let ext_url = d["url"].as_str().unwrap_or("").to_string();
            let url = if ext_url.starts_with("http") {
                ext_url
            } else {
                format!("https://reddit.com{permalink}")
            };
            Some(FusionItem {
                title,
                url,
                source: "Reddit".into(),
                raw_score: score,
            })
        })
        .collect();
    Ok(items)
}

// ─── /api/hunt/:username ──────────────────────────────────────────────────────

async fn api_hunt(
    State(s): State<Arc<WebState>>,
    Path(username): Path<String>,
) -> Json<Vec<SocialHit>> {
    // Sanitise: alphanumeric, _, -, . only
    let clean: String = username
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
        .take(50)
        .collect();
    if clean.is_empty() {
        return Json(vec![]);
    }

    const PLATFORMS: &[(&str, &str, &str)] = &[
        ("GitHub", "https://github.com/{}", "⬛"),
        ("npm", "https://www.npmjs.com/~{}", "📦"),
        ("PyPI", "https://pypi.org/user/{}/", "🐍"),
        ("DEV.to", "https://dev.to/{}", "🧑‍💻"),
        ("Medium", "https://medium.com/@{}", "📝"),
        ("Reddit", "https://www.reddit.com/user/{}/", "🔴"),
        (
            "HackerNews",
            "https://news.ycombinator.com/user?id={}",
            "🟠",
        ),
        ("Keybase", "https://keybase.io/{}", "🔑"),
        ("GitLab", "https://gitlab.com/{}", "🦊"),
        ("Codeberg", "https://codeberg.org/{}", "🧊"),
        ("SourceHut", "https://sr.ht/~{}/", "🌸"),
        ("crates.io", "https://crates.io/users/{}", "🦀"),
        ("Docker Hub", "https://hub.docker.com/u/{}/", "🐳"),
        ("Replit", "https://replit.com/@{}", "♻️"),
        ("Kaggle", "https://www.kaggle.com/{}", "📊"),
        ("Mastodon", "https://mastodon.social/@{}", "🐘"),
        ("Lobste.rs", "https://lobste.rs/u/{}", "🦞"),
        ("CodePen", "https://codepen.io/{}", "✏️"),
        ("Hashnode", "https://hashnode.com/@{}", "📰"),
        ("Speakerdeck", "https://speakerdeck.com/{}", "🎤"),
        ("Twitch", "https://www.twitch.tv/{}", "🎮"),
        ("YouTube", "https://www.youtube.com/@{}", "▶️"),
    ];

    let futs: Vec<_> = PLATFORMS
        .iter()
        .map(|&(name, url_tpl, icon)| {
            let url = url_tpl.replace("{}", &clean);
            let http = s.http.clone();
            async move {
                let result =
                    tokio::time::timeout(Duration::from_secs(5), http.head(&url).send()).await;
                let found = matches!(result, Ok(Ok(ref r)) if r.status().is_success());
                SocialHit {
                    platform: name.to_string(),
                    url,
                    found,
                    icon: icon.to_string(),
                }
            }
        })
        .collect();

    Json(futures::future::join_all(futs).await)
}

// ─── Server startup ───────────────────────────────────────────────────────────

/// Start the HTTP server.  Blocks until the server exits.
pub async fn start_server(
    alerts: AlertBuf,
    findings: FindingsBuf,
    redis_store: Option<crate::redis_store::RedisStore>,
    port: u16,
) -> Result<()> {
    let state = Arc::new(WebState::new(alerts, findings, redis_store));

    let app = Router::new()
        .route("/", get(serve_dashboard))
        .route("/api/alerts", get(api_alerts))
        .route("/api/vip", get(api_vip))
        .route("/api/social", get(api_social))
        .route("/api/feed", get(api_feed))
        .route("/api/trending", get(api_trending))
        .route("/api/leaks", get(api_leaks))
        .route("/api/ghost", get(api_ghost))
        .route("/api/twitter", get(api_twitter))
        .route("/api/reddit", get(api_reddit))
        .route("/api/secrets", get(api_secrets))
        .route("/api/newsmap", get(api_newsmap))
        .route("/api/fusion", get(api_fusion))
        .route("/api/hunt/:username", get(api_hunt))
        .route("/worldmap", get(serve_worldmap))
        .route("/api/worldevents", get(api_worldevents))
        .with_state(state)
        .layer(CorsLayer::permissive());

    let addr = format!("127.0.0.1:{port}");
    println!("🔭  repo-radar dashboard → http://localhost:{port}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
