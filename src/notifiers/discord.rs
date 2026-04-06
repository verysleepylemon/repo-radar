use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::json;

use crate::detector::{Alert, AlertPriority, AlertSource};

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
        let color: u32 = match alert.priority {
            AlertPriority::Critical => 0xFF0000, // red — sensitive content
            AlertPriority::High => 0xFF4444,
            AlertPriority::Normal if alert.score >= 1000.0 => 0xFFAA00,
            _ => 0x00BBFF,
        };

        let (title_emoji, source_label) = match &alert.source {
            AlertSource::GitHubTrending | AlertSource::SpikeDetected => ("🚀", alert.source.to_string()),
            AlertSource::HackerNews => ("🟠", "Hacker News".to_string()),
            AlertSource::Twitter => ("🐦", "Twitter / X".to_string()),
            AlertSource::Reddit => ("📱", "Reddit".to_string()),
            AlertSource::RssFeed(name) => ("📰", format!("RSS: {}", name)),
        };

        let sensitive_note = if alert.is_critical() {
            "\n\n🚨 **SENSITIVE / POTENTIALLY CENSORED CONTENT**"
        } else {
            ""
        };

        let description = format!(
            "{}{}",
            alert.description.as_deref().unwrap_or("No description"),
            sensitive_note
        );

        // Field labels adapt to the source type
        let (f1_name, f1_val, f2_name, f2_val) = if alert.is_github() {
            (
                "⭐ Stars now", alert.stars_now.to_string(),
                "📈 Stars (24h)", format!("+{}", alert.stars_gained_24h),
            )
        } else {
            (
                "👍 Likes/Upvotes", alert.stars_now.to_string(),
                "🔁 Shares/RTs", alert.forks.to_string(),
            )
        };

        let body = json!({
            "embeds": [{
                "title": format!("{} {}", title_emoji, alert.repo_full_name.chars().take(80).collect::<String>()),
                "url": alert.url,
                "description": description.chars().take(300).collect::<String>(),
                "color": color,
                "fields": [
                    {"name": f1_name, "value": f1_val, "inline": true},
                    {"name": f2_name, "value": f2_val, "inline": true},
                    {"name": "🌍 Platform", "value": alert.language.as_deref().unwrap_or("Unknown"), "inline": true},
                    {"name": "📊 Score", "value": format!("{:.0}", alert.score), "inline": true},
                    {"name": "🔎 Source", "value": source_label, "inline": true},
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
