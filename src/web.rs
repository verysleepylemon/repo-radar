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
    extract::State,
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

    // ── Reddit: public JSON, no auth needed ──────────────────────────────────
    const SUBREDDITS: &[&str] = &[
        "MachineLearning",
        "artificial",
        "LocalLLaMA",
        "programming",
        "technology",
        "netsec",
        "cybersecurity",
        "worldnews",
        "geopolitics",
        "science",
    ];
    for sub in SUBREDDITS {
        let url = format!("https://www.reddit.com/r/{sub}/hot.json?limit=25");
        if let Ok(resp) = http
            .get(&url)
            .header("User-Agent", "repo-radar/1.0 leak-monitor")
            .send()
            .await
        {
            if let Ok(data) = resp.json::<serde_json::Value>().await {
                if let Some(posts) = data["data"]["children"].as_array() {
                    for post in posts.iter().take(15) {
                        let d = &post["data"];
                        let title = d["title"].as_str().unwrap_or("").to_string();
                        let post_url = d["url"].as_str().unwrap_or("").to_string();
                        let permalink = d["permalink"].as_str().unwrap_or("");
                        let combined = format!("{title} {post_url}").to_lowercase();
                        let vip_match = VIP_TERMS
                            .iter()
                            .find(|&&t| combined.contains(t))
                            .map(|s| s.to_string());
                        let is_ai = AI_KEYWORDS.iter().any(|&kw| combined.contains(kw));
                        if vip_match.is_some() || is_ai {
                            items.push(VipItem {
                                title,
                                url: if post_url.starts_with("http") {
                                    post_url
                                } else {
                                    format!("https://reddit.com{permalink}")
                                },
                                source: format!("Reddit r/{sub}"),
                                score: d["score"].as_u64().map(|v| v as u32),
                                by: d["author"].as_str().map(Into::into),
                                vip_match,
                                time: d["created_utc"].as_f64().map(|t| t as i64),
                            });
                        }
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

    // Reddit — broad subreddit list covering tech, world news, security, science
    const SOCIAL_SUBS: &[&str] = &[
        "MachineLearning",
        "artificial",
        "LocalLLaMA",
        "programming",
        "technology",
        "compsci",
        "netsec",
        "cybersecurity",
        "worldnews",
        "news",
        "geopolitics",
        "science",
        "dataisbeautiful",
        "sysadmin",
        "devops",
    ];
    for sub in SOCIAL_SUBS {
        let url = format!("https://www.reddit.com/r/{sub}/hot.json?limit=10");
        if let Ok(resp) = http
            .get(&url)
            .header("User-Agent", "repo-radar/1.0 social-feed")
            .send()
            .await
        {
            if let Ok(data) = resp.json::<serde_json::Value>().await {
                if let Some(posts) = data["data"]["children"].as_array() {
                    for post in posts.iter().take(8) {
                        let d = &post["data"];
                        let title = d["title"].as_str().unwrap_or("").to_string();
                        let post_url = d["url"].as_str().unwrap_or("").to_string();
                        let permalink = d["permalink"].as_str().unwrap_or("");
                        let combined = format!("{title} {post_url}").to_lowercase();
                        let vip_match = VIP_TERMS
                            .iter()
                            .find(|&&t| combined.contains(t))
                            .map(|s| s.to_string());
                        items.push(VipItem {
                            title,
                            url: if post_url.starts_with("http") {
                                post_url
                            } else {
                                format!("https://reddit.com{permalink}")
                            },
                            source: format!("Reddit r/{sub}"),
                            score: d["score"].as_u64().map(|v| v as u32),
                            by: d["author"].as_str().map(Into::into),
                            vip_match,
                            time: d["created_utc"].as_f64().map(|t| t as i64),
                        });
                    }
                }
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

/// Search GitHub for repos that look like source dumps:
/// many stars, very few commits (≤ 5).  Sort by stars desc (biggest first).
async fn fetch_ghost_repos(http: &reqwest::Client) -> Result<Vec<GhostRepo>> {
    // Look back 6 months — catches recent viral dumps without being too broad
    let since = (Utc::now() - CDuration::days(180))
        .format("%Y-%m-%d")
        .to_string();

    let q = format!("stars:>50 fork:false created:>{since}");
    let resp = http
        .get("https://api.github.com/search/repositories")
        .query(&[
            ("q", q.as_str()),
            ("sort", "stars"),
            ("order", "desc"),
            ("per_page", "30"),
        ])
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("GitHub search {}", resp.status());
    }

    let data: serde_json::Value = resp.json().await?;
    let items = match data["items"].as_array() {
        Some(a) => a.clone(),
        None => return Ok(vec![]),
    };

    let mut results = Vec::new();
    for repo in &items {
        let stars = repo["stargazers_count"].as_u64().unwrap_or(0) as u32;
        let forks = repo["forks_count"].as_u64().unwrap_or(0) as u32;
        let full_name = match repo["full_name"].as_str() {
            Some(n) => n.to_string(),
            None => continue,
        };

        // Ghost heuristic: abnormally high stars-to-forks ratio
        let ghost_ratio = forks < (stars / 15).max(1) || (stars >= 500 && forks < 10);
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

async fn fetch_github_trending(http: &reqwest::Client) -> Result<serde_json::Value> {
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
        .send()
        .await?;

    if !resp.status().is_success() {
        anyhow::bail!("GitHub API {}", resp.status());
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

    // ── Reddit — broad subreddits, no filter ─────────────────────────────────
    const WORLD_SUBS: &[&str] = &[
        "worldnews",
        "news",
        "technology",
        "programming",
        "MachineLearning",
        "artificial",
        "LocalLLaMA",
        "netsec",
        "cybersecurity",
        "geopolitics",
        "science",
        "economics",
        "sysadmin",
    ];
    for sub in WORLD_SUBS {
        let url = format!("https://www.reddit.com/r/{sub}/hot.json?limit=15");
        if let Ok(resp) = http
            .get(&url)
            .header("User-Agent", "repo-radar/1.0 world-feed")
            .send()
            .await
        {
            if let Ok(data) = resp.json::<serde_json::Value>().await {
                if let Some(posts) = data["data"]["children"].as_array() {
                    for post in posts.iter().take(12) {
                        let d = &post["data"];
                        let title = d["title"].as_str().unwrap_or("").to_string();
                        let post_url = d["url"].as_str().unwrap_or("").to_string();
                        let permalink = d["permalink"].as_str().unwrap_or("");
                        let combined = format!("{title} {post_url}");
                        let (tier, tags) = classify_tier(&combined);
                        items.push(WorldFeedItem {
                            title,
                            url: if post_url.starts_with("http") {
                                post_url
                            } else {
                                format!("https://reddit.com{permalink}")
                            },
                            source: format!("Reddit r/{sub}"),
                            score: d["score"].as_u64().map(|v| v as u32),
                            by: d["author"].as_str().map(Into::into),
                            time: d["created_utc"].as_f64().map(|t| t as i64),
                            tier: tier.into(),
                            tags,
                        });
                    }
                }
            }
        }
    }

    Ok(items)
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
        .route("/api/secrets", get(api_secrets))
        .with_state(state)
        .layer(CorsLayer::permissive());

    let addr = format!("127.0.0.1:{port}");
    println!("🔭  repo-radar dashboard → http://localhost:{port}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
