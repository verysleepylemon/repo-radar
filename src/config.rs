use clap::Args;
use std::time::Duration;

/// All runtime configuration, sourced from env vars / CLI / .env file.
#[derive(Args, Debug, Clone)]
pub struct Config {
    /// Redis connection URL
    #[arg(long, env = "REDIS_URL", default_value = "redis://127.0.0.1:6379")]
    pub redis_url: String,

    /// GitHub personal access token (increases rate limit 10x)
    #[arg(long, env = "GITHUB_TOKEN")]
    pub github_token: Option<String>,

    /// Discord webhook URL for alerts
    #[arg(long, env = "DISCORD_WEBHOOK_URL")]
    pub discord_webhook_url: Option<String>,

    /// Telegram bot token
    #[arg(long, env = "TELEGRAM_BOT_TOKEN")]
    pub telegram_bot_token: Option<String>,

    /// Telegram chat / channel ID
    #[arg(long, env = "TELEGRAM_CHAT_ID")]
    pub telegram_chat_id: Option<String>,

    /// Polling interval in seconds (GitHub)
    #[arg(long, env = "POLL_INTERVAL_SECS", default_value_t = 300)]
    pub poll_interval_secs: u64,

    /// Stars gained in 24h required to trigger a spike alert
    #[arg(long, env = "SPIKE_THRESHOLD", default_value_t = 500)]
    pub spike_threshold: u64,

    /// Minimum total stars for a repo to be considered
    #[arg(long, env = "MIN_STARS", default_value_t = 50)]
    pub min_stars: u64,

    /// Hours before the same repo can be alerted again
    #[arg(long, env = "DEDUP_TTL_HOURS", default_value_t = 6)]
    pub dedup_ttl_hours: u64,

    /// Override GitHub API base URL (used in tests via mockito)
    #[arg(skip)]
    pub github_api_base: Option<String>,

    /// Override HN Algolia base URL (used in tests via mockito)
    #[arg(skip)]
    pub hn_api_base: Option<String>,

    /// Twitter / X bearer token — enables Twitter source when set
    #[arg(long, env = "TWITTER_BEARER_TOKEN")]
    pub twitter_bearer_token: Option<String>,

    /// Minimum Reddit post score to trigger an alert
    #[arg(long, env = "REDDIT_MIN_SCORE", default_value_t = 100)]
    pub reddit_min_score: u64,

    /// RSS feed polling interval in seconds
    #[arg(long, env = "RSS_INTERVAL_SECS", default_value_t = 600)]
    pub rss_interval_secs: u64,

    /// Twitter polling interval in seconds
    #[arg(long, env = "TWITTER_INTERVAL_SECS", default_value_t = 900)]
    pub twitter_interval_secs: u64,

    /// Override Twitter API base URL (used in tests)
    #[arg(skip)]
    pub twitter_api_base: Option<String>,
}

impl Config {
    /// Returns the dedup TTL as a `std::time::Duration`.
    pub fn dedup_ttl(&self) -> Duration {
        Duration::from_secs(self.dedup_ttl_hours * 3600)
    }
}
