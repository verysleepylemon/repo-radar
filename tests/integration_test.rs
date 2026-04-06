use mockito::Server;
use repo_radar::{
    config::Config,
    detector::{Alert, AlertSource},
    sources::github::GitHubSource,
    sources::hackernews::HackerNewsSource,
};

fn test_config(server_url: &str) -> Config {
    Config {
        redis_url: "redis://127.0.0.1:6379".to_string(),
        github_token: None,
        discord_webhook_url: None,
        telegram_bot_token: None,
        telegram_chat_id: None,
        poll_interval_secs: 60,
        spike_threshold: 100,
        min_stars: 10,
        dedup_ttl_hours: 1,
        github_api_base: Some(server_url.to_string()),
        hn_api_base: None,
    }
}

// ─── GitHub source ────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_fetch_trending_parses_response() {
    let mut server = Server::new_async().await;
    let body = serde_json::json!({
        "total_count": 2,
        "incomplete_results": false,
        "items": [
            {"full_name": "rust-lang/rust", "stargazers_count": 90000},
            {"full_name": "tokio-rs/tokio", "stargazers_count": 25000}
        ]
    });

    let _mock = server
        .mock("GET", mockito::Matcher::Any)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body.to_string())
        .create_async()
        .await;

    let config = test_config(&server.url());
    let source = GitHubSource::new(&config).unwrap();
    let results = source.fetch_trending(10, 30).await.unwrap();

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].full_name, "rust-lang/rust");
    assert_eq!(results[0].stars, 90000);
}

#[tokio::test]
async fn test_fetch_repo_info_parses_response() {
    let mut server = Server::new_async().await;
    let body = serde_json::json!({
        "full_name": "lemwaiping123-eng/repo-radar",
        "description": "Real-time trend detector",
        "language": "Rust",
        "stargazers_count": 42,
        "forks_count": 3,
        "html_url": "https://github.com/lemwaiping123-eng/repo-radar"
    });

    let _mock = server
        .mock("GET", "/repos/lemwaiping123-eng/repo-radar")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body.to_string())
        .create_async()
        .await;

    let config = test_config(&server.url());
    let source = GitHubSource::new(&config).unwrap();
    let info = source
        .fetch_repo_info("lemwaiping123-eng/repo-radar")
        .await
        .unwrap();

    assert_eq!(info.full_name, "lemwaiping123-eng/repo-radar");
    assert_eq!(info.stargazers_count, 42);
    assert_eq!(info.language, Some("Rust".to_string()));
}

// ─── HN source ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_fetch_hot_show_hn_parses_response() {
    let mut server = Server::new_async().await;
    let now_ts = chrono::Utc::now().timestamp();
    let body = serde_json::json!({
        "hits": [
            {
                "objectID": "12345",
                "title": "Show HN: repo-radar – real-time trend detector in Rust",
                "url": "https://github.com/lemwaiping123-eng/repo-radar",
                "points": 87,
                "num_comments": 23,
                "created_at_i": now_ts
            }
        ]
    });

    let _mock = server
        .mock("GET", mockito::Matcher::Any)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body.to_string())
        .create_async()
        .await;

    let source = HackerNewsSource::new_with_base_url(&server.url());
    let stories = source.fetch_hot_show_hn(10).await.unwrap();

    assert_eq!(stories.len(), 1);
    assert_eq!(stories[0].id, "12345");
    assert_eq!(stories[0].points, 87);
    assert!(stories[0].url.as_deref().unwrap().contains("github.com"));
}

// ─── Alert serialization ──────────────────────────────────────────────────────

#[test]
fn test_alert_serde_roundtrip() {
    let alert = Alert {
        repo_full_name: "test/repo".to_string(),
        description: Some("A test repo".to_string()),
        language: Some("Rust".to_string()),
        stars_now: 1500,
        stars_gained_24h: 600,
        forks: 45,
        growth_factor: 0.67,
        score: 660.0,
        detected_at: chrono::Utc::now(),
        source: AlertSource::SpikeDetected,
        url: "https://github.com/test/repo".to_string(),
    };

    let json = serde_json::to_string(&alert).expect("serialization failed");
    let decoded: Alert = serde_json::from_str(&json).expect("deserialization failed");

    assert_eq!(decoded.repo_full_name, alert.repo_full_name);
    assert_eq!(decoded.stars_now, alert.stars_now);
    assert_eq!(decoded.stars_gained_24h, alert.stars_gained_24h);
    assert!((decoded.score - alert.score).abs() < 0.01);
}

// ─── Spike scoring ────────────────────────────────────────────────────────────

#[test]
fn test_spike_score_formula() {
    // score = velocity + (growth_factor * 100)
    let velocity = 500u64;
    let total = 2000u64;
    let growth_factor = velocity as f64 / (total as f64 - velocity as f64);
    let score = velocity as f64 + (growth_factor * 100.0);
    assert!(score > 500.0);
    assert!(growth_factor > 0.0 && growth_factor < 1.0);
}
