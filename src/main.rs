use anyhow::Result;
use clap::{Parser, Subcommand};
use std::time::Duration;
use tokio::time;
use tracing::info;

use repo_radar::config::Config;
use repo_radar::detector::Detector;
use repo_radar::notifiers::NotifierSet;
use repo_radar::redis_store::RedisStore;
use repo_radar::secret_scanner::{new_findings_buf, SecretScanner};
use repo_radar::sources::github::GitHubSource;
use repo_radar::sources::hackernews::HackerNewsSource;
use repo_radar::sources::rss::RssSource;
use repo_radar::sources::twitter::TwitterSource;
use repo_radar::web;

#[derive(Parser)]
#[command(
    name = "repo-radar",
    version,
    about = "Real-time GitHub trend detector"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    #[command(flatten)]
    config: Config,
}

#[derive(Subcommand)]
enum Commands {
    /// Watch GitHub + HN continuously and send alerts
    Watch,
    /// Watch + serve the live web dashboard (default port 8080)
    Serve {
        /// HTTP port for the dashboard
        #[arg(long, default_value = "8080")]
        port: u16,
    },
    /// One-shot spike check for a single repo (e.g. rust-lang/rust)
    Check { repo: String },
    /// Show the last 20 alerts stored in Redis
    Status,
    /// Health-check all dependencies and data sources
    Doctor,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    let cli = Cli::parse();

    // In serve mode don't pollute the terminal with INFO detector noise;
    // everything is visible through the web dashboard instead.
    let default_filter = match &cli.command {
        Commands::Serve { .. } => "warn",
        _ => "repo_radar=info",
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| default_filter.into()),
        )
        .init();

    match cli.command {
        Commands::Watch => run_watch(cli.config).await?,
        Commands::Serve { port } => run_serve(cli.config, port).await?,
        Commands::Check { repo } => run_check(cli.config, &repo).await?,
        Commands::Status => run_status(cli.config).await?,
        Commands::Doctor => run_doctor(cli.config).await?,
    }

    Ok(())
}

async fn run_watch(config: Config) -> Result<()> {
    let store = RedisStore::try_connect(&config.redis_url).await;
    if store.is_none() {
        eprintln!("⚠️  Redis not available — running without dedup/persistence. Install Redis or set REDIS_URL.");
    }
    let notifiers = NotifierSet::from_config(&config);
    let github = GitHubSource::new(&config)?;
    let hn = HackerNewsSource::new();
    let rss = RssSource::new();
    let twitter = config
        .twitter_bearer_token
        .as_deref()
        .map(TwitterSource::new);

    let detector = Detector::new(config.clone(), store, notifiers);

    info!("repo-radar watching... press Ctrl-C to stop");
    if twitter.is_none() {
        info!("Twitter/X source disabled (set TWITTER_BEARER_TOKEN to enable)");
    }

    let poll_gh = Duration::from_secs(config.poll_interval_secs);
    let poll_hn = Duration::from_secs(180);
    let poll_rss = Duration::from_secs(config.rss_interval_secs);
    let poll_tw = Duration::from_secs(config.twitter_interval_secs);

    run_watch_loop(
        config, detector, github, hn, rss, twitter, poll_gh, poll_hn, poll_rss, poll_tw,
    )
    .await
}

async fn run_serve(config: Config, port: u16) -> Result<()> {
    let buf = web::new_alert_buf();
    let findings = new_findings_buf();

    let store = RedisStore::try_connect(&config.redis_url).await;
    if store.is_none() {
        eprintln!("⚠️  Redis not available — running without dedup/persistence.");
    }
    let notifiers = NotifierSet::from_config(&config);
    let github = GitHubSource::new(&config)?;
    let hn = HackerNewsSource::new();
    let rss = RssSource::new();
    let twitter = config
        .twitter_bearer_token
        .as_deref()
        .map(TwitterSource::new);

    let store_web = store.clone();
    let detector = Detector::new(config.clone(), store, notifiers).with_alert_buf(buf.clone());

    // Build the secret scanner and run it as a background task.
    let scanner_http = reqwest::Client::builder()
        .user_agent("repo-radar/1.0 (github.com/lemwaiping123-eng/repo-radar)")
        .timeout(std::time::Duration::from_secs(12))
        .build()?;
    let scanner = std::sync::Arc::new(SecretScanner::new(scanner_http, findings.clone())?);
    tokio::spawn(async move { scanner.run_forever().await });

    println!("🔭  repo-radar dashboard → http://localhost:{port}");

    let poll_gh = Duration::from_secs(config.poll_interval_secs);
    let poll_hn = Duration::from_secs(180);
    let poll_rss = Duration::from_secs(config.rss_interval_secs);
    let poll_tw = Duration::from_secs(config.twitter_interval_secs);

    // Background task: purge Redis alerts older than 3 days every 6 hours.
    if let Some(store_purge) = store_web.clone() {
        tokio::spawn(async move {
            let mut interval = time::interval(Duration::from_secs(6 * 3600));
            loop {
                interval.tick().await;
                match store_purge
                    .purge_old_alerts(Duration::from_secs(3 * 86_400))
                    .await
                {
                    Ok(n) if n > 0 => {
                        info!(removed = n, "Purged old Redis alerts (3-day retention)")
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "Redis purge failed"),
                }
            }
        });
    }

    // Run the web server concurrently with the watch loop.
    tokio::select! {
        res = web::start_server(buf, findings, store_web, port) => {
            if let Err(e) = res { tracing::error!(error = %e, "Web server error"); }
        }
        res = run_watch_loop(config, detector, github, hn, rss, twitter,
                             poll_gh, poll_hn, poll_rss, poll_tw) => {
            if let Err(e) = res { tracing::error!(error = %e, "Watch loop error"); }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_watch_loop(
    _config: Config,
    detector: Detector,
    github: GitHubSource,
    hn: HackerNewsSource,
    rss: RssSource,
    twitter: Option<TwitterSource>,
    poll_gh: Duration,
    poll_hn: Duration,
    poll_rss: Duration,
    poll_tw: Duration,
) -> Result<()> {
    let mut gh_interval = time::interval(poll_gh);
    let mut hn_interval = time::interval(poll_hn);
    let mut rss_interval = time::interval(poll_rss);
    let mut tw_interval = time::interval(poll_tw);

    loop {
        tokio::select! {
            _ = gh_interval.tick() => {
                if let Err(e) = detector.scan_github(&github).await {
                    tracing::warn!(error = %e, "GitHub scan error");
                }
            }
            _ = hn_interval.tick() => {
                if let Err(e) = detector.scan_hackernews(&hn).await {
                    tracing::warn!(error = %e, "HN scan error");
                }
            }
            _ = rss_interval.tick() => {
                if let Err(e) = detector.scan_rss(&rss).await {
                    tracing::warn!(error = %e, "RSS scan error");
                }
            }
            _ = tw_interval.tick() => {
                if let Some(ref tw) = twitter {
                    if let Err(e) = detector.scan_twitter(tw).await {
                        tracing::warn!(error = %e, "Twitter scan error");
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                info!("Shutting down");
                break;
            }
        }
    }
    Ok(())
}

async fn run_check(config: Config, repo: &str) -> Result<()> {
    let store = RedisStore::try_connect(&config.redis_url).await;
    let notifiers = NotifierSet::from_config(&config);
    let github = GitHubSource::new(&config)?;
    let detector = Detector::new(config, store, notifiers);

    println!("Checking {repo}...");
    match detector.check_repo(&github, repo).await? {
        Some(alert) => {
            println!("🚀 SPIKE DETECTED!");
            println!("  Stars now:   {}", alert.stars_now);
            println!("  Stars (24h): +{}", alert.stars_gained_24h);
            println!("  Score:       {:.0}", alert.score);
            println!("  URL:         {}", alert.url);
        }
        None => println!("No spike detected for {repo}"),
    }

    Ok(())
}

async fn run_status(config: Config) -> Result<()> {
    let store = match RedisStore::try_connect(&config.redis_url).await {
        Some(s) => s,
        None => {
            println!("Redis unavailable — no stored alerts to show.");
            return Ok(());
        }
    };
    let alerts = store.get_recent_alerts(20).await?;

    if alerts.is_empty() {
        println!("No alerts yet.");
        return Ok(());
    }

    println!(
        "{:<40} {:>8} {:>10} {:>8}",
        "Repo", "Stars", "+24h", "Score"
    );
    println!("{}", "-".repeat(70));
    for a in alerts {
        println!(
            "{:<40} {:>8} {:>10} {:>8.0}",
            a.repo_full_name, a.stars_now, a.stars_gained_24h, a.score
        );
    }

    Ok(())
}

/// Health-check all data sources and dependencies.
/// Inspired by `claw doctor` from ultraworkers/claw-code — a startup sanity
/// check that surfaces broken deps before the main loop runs.
async fn run_doctor(config: Config) -> Result<()> {
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .user_agent("repo-radar/doctor")
        .build()?;

    println!("repo-radar doctor — checking dependencies\n");

    // ── Redis ────────────────────────────────────────────────────────────────
    let redis_ok = RedisStore::connect(&config.redis_url).await.is_ok();
    print_check("Redis", &config.redis_url, redis_ok, None);

    // ── GitHub API ───────────────────────────────────────────────────────────
    let gh_result = {
        let mut req = http.get("https://api.github.com/rate_limit");
        if let Some(ref tok) = config.github_token {
            req = req.bearer_auth(tok);
        }
        req.send().await
    };
    let (gh_ok, gh_note) = match gh_result {
        Ok(r) if r.status().is_success() => {
            let remaining = r
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| v["rate"]["remaining"].as_u64())
                .map(|n| format!("{n} requests remaining"))
                .unwrap_or_default();
            (true, Some(remaining))
        }
        Ok(r) => (false, Some(format!("HTTP {}", r.status()))),
        Err(e) => (false, Some(e.to_string())),
    };
    let gh_token_label = if config.github_token.is_some() {
        "authenticated"
    } else {
        "unauthenticated (set GITHUB_TOKEN for 5000 req/hr)"
    };
    print_check("GitHub API", gh_token_label, gh_ok, gh_note.as_deref());

    // ── HackerNews Firebase ───────────────────────────────────────────────────
    let hn_ok = http
        .get("https://hacker-news.firebaseio.com/v0/topstories.json?limitToFirst=1&orderBy=%22$key%22")
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);
    print_check(
        "HackerNews Firebase",
        "hacker-news.firebaseio.com",
        hn_ok,
        None,
    );

    // ── NPM Registry ─────────────────────────────────────────────────────────
    let npm_ok = http
        .get("https://registry.npmjs.org/@anthropic-ai%2Fclaude-code/latest")
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);
    print_check("NPM Registry", "registry.npmjs.org", npm_ok, None);

    // ── RSS (HN via Algolia) ──────────────────────────────────────────────────
    let rss_ok = http
        .get("https://hn.algolia.com/api/v1/search_by_date?tags=story&hitsPerPage=1")
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);
    print_check("HN Algolia RSS", "hn.algolia.com", rss_ok, None);

    // ── Twitter/X ─────────────────────────────────────────────────────────────
    let tw_configured = config.twitter_bearer_token.is_some();
    print_check(
        "Twitter/X",
        if tw_configured {
            "token configured"
        } else {
            "no TWITTER_BEARER_TOKEN (optional)"
        },
        tw_configured,
        if tw_configured {
            None
        } else {
            Some("disabled — set TWITTER_BEARER_TOKEN to enable")
        },
    );

    // ── Environment summary ────────────────────────────────────────────────────
    println!("\n── env tokens ──────────────────────────────────────────────");
    println!(
        "  GITHUB_TOKEN         {}",
        if config.github_token.is_some() {
            "set ✓"
        } else {
            "unset  (60 req/hr limit applies)"
        }
    );
    println!(
        "  TWITTER_BEARER_TOKEN {}",
        if config.twitter_bearer_token.is_some() {
            "set ✓"
        } else {
            "unset  (Twitter feed disabled)"
        }
    );
    println!(
        "  DISCORD_WEBHOOK_URL  {}",
        if config.discord_webhook_url.is_some() {
            "set ✓"
        } else {
            "unset  (Discord alerts disabled)"
        }
    );
    println!(
        "  TELEGRAM_BOT_TOKEN   {}",
        if config.telegram_bot_token.is_some() {
            "set ✓"
        } else {
            "unset  (Telegram alerts disabled)"
        }
    );
    println!("  REDIS_URL            {}", config.redis_url);

    // ── Routes ────────────────────────────────────────────────────────────────
    println!("\n── live routes (serve mode) ────────────────────────────────");
    let routes = [
        "/               → dashboard",
        "/api/alerts     → trend alerts (Redis)",
        "/api/vip        → VIP AI feed",
        "/api/social     → social pulse",
        "/api/feed       → world feed (classified)",
        "/api/trending   → GitHub trending",
        "/api/leaks      → leak tracker + NPM monitor",
        "/api/ghost      → ghost accounts",
        "/api/twitter    → Twitter/X (requires token)",
        "/api/viral      → Hot Posts (HN Algolia + Lobsters)",
        "/api/secrets    → secret scanner",
        "/api/newsmap    → tech news map",
        "/api/fusion     → signal fusion",
        "/api/hunt/:u    → social hunt",
    ];
    for r in routes {
        println!("  {r}");
    }

    println!();
    Ok(())
}

fn print_check(name: &str, target: &str, ok: bool, note: Option<&str>) {
    let icon = if ok { "✓" } else { "✗" };
    let note_str = note.map(|n| format!(" — {n}")).unwrap_or_default();
    println!("  [{icon}] {name:<22} {target}{note_str}");
}
