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
];

// ─── Shared state ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct WebState {
    pub alerts: AlertBuf,
    pub http: reqwest::Client,
    vip_cache: Arc<RwLock<Option<(Instant, Vec<VipItem>)>>>,
    trend_cache: Arc<RwLock<Option<(Instant, serde_json::Value)>>>,
}

impl WebState {
    pub fn new(alerts: AlertBuf) -> Self {
        let http = reqwest::Client::builder()
            .user_agent("repo-radar/1.0 (github.com/lemwaiping123-eng/repo-radar)")
            .timeout(Duration::from_secs(12))
            .build()
            .unwrap_or_default();
        Self {
            alerts,
            http,
            vip_cache: Arc::new(RwLock::new(None)),
            trend_cache: Arc::new(RwLock::new(None)),
        }
    }
}

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

// ─── Route handlers ───────────────────────────────────────────────────────────

async fn serve_dashboard() -> Html<&'static str> {
    Html(include_str!("../assets/index.html"))
}

async fn api_alerts(State(s): State<Arc<WebState>>) -> Json<Vec<Alert>> {
    let g = s.alerts.read().await;
    Json(g.iter().cloned().collect())
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
    let items = fetch_vip_feed(&s.http).await.unwrap_or_default();
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

// ─── Data helpers ─────────────────────────────────────────────────────────────

async fn fetch_vip_feed(http: &reqwest::Client) -> Result<Vec<VipItem>> {
    let ids: Vec<u64> = http
        .get("https://hacker-news.firebaseio.com/v0/topstories.json")
        .send()
        .await?
        .json()
        .await?;

    let mut items: Vec<VipItem> = Vec::new();

    // Fetch first 120 IDs in parallel, 20 at a time.
    for chunk in ids.iter().take(120).collect::<Vec<_>>().chunks(20) {
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
                    url: if story_url.is_empty() { fallback } else { story_url },
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

    // VIP matches first, then by score descending.
    items.sort_by(|a, b| {
        let av = a.vip_match.is_some() as u8;
        let bv = b.vip_match.is_some() as u8;
        bv.cmp(&av).then_with(|| b.score.cmp(&a.score))
    });
    items.truncate(30);
    Ok(items)
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

// ─── Server startup ───────────────────────────────────────────────────────────

/// Start the HTTP server.  Blocks until the server exits.
pub async fn start_server(alerts: AlertBuf, port: u16) -> Result<()> {
    let state = Arc::new(WebState::new(alerts));

    let app = Router::new()
        .route("/", get(serve_dashboard))
        .route("/api/alerts", get(api_alerts))
        .route("/api/vip", get(api_vip))
        .route("/api/trending", get(api_trending))
        .with_state(state)
        .layer(CorsLayer::permissive());

    let addr = format!("127.0.0.1:{port}");
    println!("🔭  repo-radar dashboard → http://localhost:{port}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
