use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::json;

use crate::detector::{Alert, AlertPriority, AlertSource};

/// Sends a Telegram message via Bot API when a trending spike is detected.
#[derive(Clone)]
pub struct TelegramNotifier {
    bot_token: String,
    chat_id: String,
    client: Client,
}

impl TelegramNotifier {
    pub fn new(bot_token: &str, chat_id: &str) -> Self {
        Self {
            bot_token: bot_token.to_string(),
            chat_id: chat_id.to_string(),
            client: Client::new(),
        }
    }

    pub async fn send(&self, alert: &Alert) -> Result<()> {
        let header_emoji = match alert.priority {
            AlertPriority::Critical => "🚨",
            AlertPriority::High => "🔴",
            AlertPriority::Normal => match &alert.source {
                AlertSource::GitHubTrending | AlertSource::SpikeDetected => "🚀",
                AlertSource::HackerNews => "🟠",
                AlertSource::Twitter => "🐦",
                AlertSource::Reddit => "📱",
                AlertSource::RssFeed(_) => "📰",
            },
        };

        let sensitive_banner = if alert.is_critical() {
            "🚨 *SENSITIVE / POTENTIALLY CENSORED*\n"
        } else {
            ""
        };

        let desc = alert.description.as_deref().unwrap_or("No description");

        let text = format!(
            "{emoji} *{title}*\n\
             {banner}\
             _{desc}_\n\n\
             👍 {likes}  🔁 {shares}  📊 {score:.0}\n\
             🔎 {source}\n\n\
             🔗 {url}",
            emoji = header_emoji,
            title = escape_markdown(&alert.repo_full_name.chars().take(80).collect::<String>()),
            banner = sensitive_banner,
            desc = escape_markdown(&desc.chars().take(200).collect::<String>()),
            likes = alert.stars_now,
            shares = alert.forks,
            score = alert.score,
            source = escape_markdown(&alert.source.to_string()),
            url = alert.url,
        );

        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token);

        let body = json!({
            "chat_id": self.chat_id,
            "text": text,
            "parse_mode": "MarkdownV2",
            "disable_web_page_preview": false,
        });

        self.client
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("Telegram POST failed")?
            .error_for_status()
            .context("Telegram returned non-2xx")?;

        Ok(())
    }
}

/// Escape special chars for Telegram MarkdownV2.
fn escape_markdown(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if "_*[]()~`>#+-=|{}.!\\".contains(c) {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect()
}
