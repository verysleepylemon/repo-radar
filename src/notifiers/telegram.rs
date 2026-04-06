use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::json;

use crate::detector::Alert;

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
        let stars_emoji = if alert.stars_gained_24h >= 1000 {
            "🔥🔥🔥"
        } else if alert.stars_gained_24h >= 500 {
            "🔥🔥"
        } else {
            "🔥"
        };

        let text = format!(
            "{stars_emoji} *{repo}*\n\
             _{desc}_\n\n\
             ⭐ {stars_now} stars (+{gained} in 24h)\n\
             🍴 {forks} forks  |  💻 {lang}\n\
             📊 Score: {score:.0}  |  🔎 {source:?}\n\n\
             🔗 {url}",
            stars_emoji = stars_emoji,
            repo = escape_markdown(&alert.repo_full_name),
            desc = escape_markdown(alert.description.as_deref().unwrap_or("No description")),
            stars_now = alert.stars_now,
            gained = alert.stars_gained_24h,
            forks = alert.forks,
            lang = alert.language.as_deref().unwrap_or("Unknown"),
            score = alert.score,
            source = alert.source,
            url = alert.url,
        );

        let url = format!(
            "https://api.telegram.org/bot{}/sendMessage",
            self.bot_token
        );

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
