pub mod discord;
pub mod telegram;
pub mod windows_toast;

use tracing::warn;

use crate::config::Config;
use crate::detector::Alert;
use discord::DiscordNotifier;
use telegram::TelegramNotifier;

/// Aggregates zero or more active notifiers and fans alerts out to all of them.
#[derive(Clone)]
pub struct NotifierSet {
    discord: Option<DiscordNotifier>,
    telegram: Option<TelegramNotifier>,
}

impl NotifierSet {
    /// Build from config — skips notifiers whose credentials are absent.
    pub fn from_config(config: &Config) -> Self {
        let discord = config
            .discord_webhook_url
            .as_deref()
            .map(DiscordNotifier::new);

        let telegram = match (&config.telegram_bot_token, &config.telegram_chat_id) {
            (Some(token), Some(chat_id)) => Some(TelegramNotifier::new(token, chat_id)),
            _ => None,
        };

        Self { discord, telegram }
    }

    /// Send `alert` to all configured notifiers. Errors are logged but don't
    /// prevent other notifiers from running.
    pub async fn notify(&self, alert: &Alert) {
        if let Some(ref d) = self.discord {
            if let Err(e) = d.send(alert).await {
                warn!(error = %e, "Discord notification failed");
            }
        }
        if let Some(ref t) = self.telegram {
            if let Err(e) = t.send(alert).await {
                warn!(error = %e, "Telegram notification failed");
            }
        }
    }
}
