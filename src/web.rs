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
use crate::secret_scanner::FindingsBuf;

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
    pub alerts:      AlertBuf,
    pub findings:    FindingsBuf,
    pub http:        reqwest::Client,
    pub redis_store: Option<crate::redis_store::RedisStore>,
    vip_cache:       Arc<RwLock<Option<(Instant, Vec<VipItem>)>>>,
    trend_cache:     Arc<RwLock<Option<(Instant, serde_json::Value)>>>,
    leak_cache:      Arc<RwLock<Option<(Instant, Vec<LeakItem>)>>>,
}

impl WebState {
    pub fn new(alerts: AlertBuf, findings: FindingsBuf, redis_store: Option<crate::redis_store::RedisStore>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent("repo-radar/1.0 (github.com/lemwaiping123-eng/repo-radar)")
            .timeout(Duration::from_secs(12))
            .build()
            .unwrap_or_default();
        Self {
            alerts,
            findings,
            http,
            redis_store,
            vip_cache:       Arc::new(RwLock::new(None)),
            trend_cache:     Arc::new(RwLock::new(None)),
            leak_cache:      Arc::new(RwLock::new(None)),
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
    let g = s.alerts.read().await;
    Json(g.iter().cloned().collect())
}

async fn api_secrets(State(s): State<Arc<WebState>>) -> Json<Vec<serde_json::Value>> {
    // Rust in-memory findings
    let rust_findings: Vec<serde_json::Value> = {
        let g = s.findings.read().await;
        g.iter().filter_map(|f| serde_json::to_value(f).ok()).collect()
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
        let id = f.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
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
            if !combined.contains("leak") && !combined.contains("source-map") && !combined.contains("sourcemap") {
                return None;
            }
            let owner = r["owner"]["login"].as_str().unwrap_or("unknown").to_string();
            let created = r["created_at"].as_str().unwrap_or("").to_string();
            let mut tags = vec!["GitHub".to_string()];
            if let Some(l) = &lang { tags.push(l.clone()); }
            if combined.contains("leak") { tags.push("Leaked".into()); }
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
                clone_cmd: if clone_url.is_empty() { format!("git clone {html_url}") } else { format!("git clone {clone_url}") },
                mirrors: vec![html_url.clone()],
                npm_pkg: None,
                tags,
                language: lang,
                confirmed: false,
                severity: if stars > 100 { "high".into() } else { "medium".into() },
                repo_url: html_url,
            })
        })
        .collect())
}

// ─── Server startup ───────────────────────────────────────────────────────────

/// Start the HTTP server.  Blocks until the server exits.
pub async fn start_server(alerts: AlertBuf, findings: FindingsBuf, redis_store: Option<crate::redis_store::RedisStore>, port: u16) -> Result<()> {
    let state = Arc::new(WebState::new(alerts, findings, redis_store));

    let app = Router::new()
        .route("/", get(serve_dashboard))
        .route("/api/alerts",  get(api_alerts))
        .route("/api/vip",     get(api_vip))
        .route("/api/trending",get(api_trending))
        .route("/api/leaks",   get(api_leaks))
        .route("/api/secrets", get(api_secrets))
        .with_state(state)
        .layer(CorsLayer::permissive());

    let addr = format!("127.0.0.1:{port}");
    println!("🔭  repo-radar dashboard → http://localhost:{port}");

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
