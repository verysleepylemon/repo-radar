use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::json;

use crate::detector::Alert;

/// Posts a rich Discord embed when a trending spike is detected.
#[derive(Clone)]
pub struct DiscordNotifier {
    webhook_url: String,
    client: Client,
}

impl DiscordNotifier {
    pub fn new(webhook_url: &str) -> Self {
        Self {
            webhook_url: webhook_url.to_string(),
            client: Client::new(),
        }
    }

    pub async fn send(&self, alert: &Alert) -> Result<()> {
        let color = if alert.score >= 1000 {
            0xFF4444 // red — extreme spike
        } else if alert.score >= 500 {
            0xFFAA00 // orange — hot
        } else {
            0x00BBFF // blue — notable
        };

        let body = json!({
            "embeds": [{
                "title": format!("🚀 {}", alert.repo_full_name),
                "url": alert.url,
                "description": alert.description.as_deref().unwrap_or("No description"),
                "color": color,
                "fields": [
                    {"name": "⭐ Stars now", "value": alert.stars_now.to_string(), "inline": true},
                    {"name": "📈 Stars (24h)", "value": format!("+{}", alert.stars_gained_24h), "inline": true},
                    {"name": "🍴 Forks", "value": alert.forks.to_string(), "inline": true},
                    {"name": "💻 Language", "value": alert.language.as_deref().unwrap_or("Unknown"), "inline": true},
                    {"name": "📊 Score", "value": format!("{:.0}", alert.score), "inline": true},
                    {"name": "🔎 Source", "value": format!("{:?}", alert.source), "inline": true},
                ],
                "timestamp": alert.detected_at.to_rfc3339(),
                "footer": {"text": "repo-radar • real-time trend detection"}
            }]
        });

        self.client
            .post(&self.webhook_url)
            .json(&body)
            .send()
            .await
            .context("Discord POST failed")?
            .error_for_status()
            .context("Discord returned non-2xx")?;

        Ok(())
    }
}
