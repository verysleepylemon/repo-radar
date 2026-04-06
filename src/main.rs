use anyhow::Result;
use clap::{Parser, Subcommand};
use std::time::Duration;
use tokio::time;
use tracing::info;

use repo_radar::config::Config;
use repo_radar::detector::Detector;
use repo_radar::notifiers::NotifierSet;
use repo_radar::redis_store::RedisStore;
use repo_radar::sources::github::GitHubSource;
use repo_radar::sources::hackernews::HackerNewsSource;
use repo_radar::sources::reddit::RedditSource;
use repo_radar::sources::rss::RssSource;
use repo_radar::sources::twitter::TwitterSource;

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
    /// One-shot spike check for a single repo (e.g. rust-lang/rust)
    Check { repo: String },
    /// Show the last 20 alerts stored in Redis
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "repo_radar=info".into()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Watch => run_watch(cli.config).await?,
        Commands::Check { repo } => run_check(cli.config, &repo).await?,
        Commands::Status => run_status(cli.config).await?,
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
    let reddit = RedditSource::new(config.reddit_min_score);
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

    let poll_gh   = Duration::from_secs(config.poll_interval_secs);
    let poll_hn   = Duration::from_secs(180);
    let poll_rss  = Duration::from_secs(config.rss_interval_secs);
    let poll_tw   = Duration::from_secs(config.twitter_interval_secs);
    let poll_rd   = Duration::from_secs(900);

    let mut gh_interval = time::interval(poll_gh);
    let mut hn_interval = time::interval(poll_hn);
    let mut rss_interval = time::interval(poll_rss);
    let mut tw_interval = time::interval(poll_tw);
    let mut rd_interval = time::interval(poll_rd);

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
            _ = rd_interval.tick() => {
                if let Err(e) = detector.scan_reddit(&reddit).await {
                    tracing::warn!(error = %e, "Reddit scan error");
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
