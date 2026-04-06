//! Real-time GitHub secret scanner.
//!
//! Polls the public GitHub Events API every 30 seconds, looks at PushEvent
//! commits, fetches their diffs, and scans every patch hunk with a library of
//! regex patterns that match common API key / credential formats.
//!
//! Findings are stored in a shared in-memory ring-buffer; the web dashboard
//! exposes them at `/api/secrets`.
//!
//! ⚠️  RESPONSIBLE DISCLOSURE ONLY — secrets are masked before storage.
//!     The full credential value is NEVER saved or transmitted.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};

// ─── Public buffer type ───────────────────────────────────────────────────────

pub const MAX_FINDINGS: usize = 200;
pub type FindingsBuf = Arc<RwLock<VecDeque<SecretFinding>>>;

pub fn new_findings_buf() -> FindingsBuf {
    Arc::new(RwLock::new(VecDeque::with_capacity(MAX_FINDINGS)))
}

// ─── Detection patterns ───────────────────────────────────────────────────────

/// (label, regex_pattern, severity)
static PATTERNS: &[(&str, &str, &str)] = &[
    // AWS
    ("AWS Access Key ID",          r"AKIA[0-9A-Z]{16}",                                                        "critical"),
    ("AWS Secret Access Key",      r"(?i)aws.{0,30}secret.{0,30}[A-Za-z0-9/+=]{40}",                          "critical"),

    // GitHub
    ("GitHub Personal Token",      r"ghp_[a-zA-Z0-9]{36}",                                                     "critical"),
    ("GitHub Fine-grained Token",  r"github_pat_[a-zA-Z0-9_]{82}",                                             "critical"),
    ("GitHub App Install Token",   r"ghs_[a-zA-Z0-9]{36}",                                                     "high"),
    ("GitHub OAuth Token",         r"gho_[a-zA-Z0-9]{36}",                                                     "high"),

    // OpenAI / Anthropic / AI
    ("OpenAI API Key",             r"sk-[a-zA-Z0-9]{48}",                                                      "critical"),
    ("Anthropic API Key",          r"sk-ant-api\d{2}-[a-zA-Z0-9_-]{93}",                                      "critical"),
    ("Anthropic Key (short)",      r"sk-ant-[a-zA-Z0-9_-]{40,}",                                               "critical"),
    ("Cohere API Key",             r"cohere[_\-\s]*[a-zA-Z0-9]{40}",                                            "high"),

    // Stripe
    ("Stripe Live Secret Key",     r"sk_live_[a-zA-Z0-9]{24,}",                                                "critical"),
    ("Stripe Test Secret Key",     r"sk_test_[a-zA-Z0-9]{24,}",                                                "medium"),
    ("Stripe Restricted Key",      r"rk_live_[a-zA-Z0-9]{24,}",                                                "high"),

    // Google
    ("Google API Key",             r"AIza[0-9A-Za-z\-_]{35}",                                                  "high"),
    ("Google OAuth Client ID",     r"[0-9]+-[a-z0-9]+\.apps\.googleusercontent\.com",                          "medium"),

    // Slack
    ("Slack Bot Token",            r"xoxb-[0-9]{10,}-[0-9]{10,}-[a-zA-Z0-9]{24}",                             "high"),
    ("Slack User Token",           r"xoxp-[0-9]{10,}-[0-9]{10,}-[0-9]{10,}-[a-zA-Z0-9]{32}",                 "high"),
    ("Slack Webhook",              r"hooks\.slack\.com/services/T[a-zA-Z0-9_]{8}/B[a-zA-Z0-9_]{8}/[a-zA-Z0-9_]{24}", "high"),

    // Twilio
    ("Twilio Account SID",         r"AC[a-f0-9]{32}",                                                          "medium"),
    ("Twilio Auth Token",          r"(?i)twilio.{0,30}[a-f0-9]{32}",                                           "high"),

    // Discord
    ("Discord Bot Token",          r"[MN][a-zA-Z0-9]{23}\.[a-zA-Z0-9\-_]{6}\.[a-zA-Z0-9\-_]{27}",            "high"),
    ("Discord Webhook",            r"discord(?:app)?\.com/api/webhooks/[0-9]{18}/[a-zA-Z0-9_\-]{68}",          "high"),

    // SendGrid / Mailgun / Mailchimp
    ("SendGrid API Key",           r"SG\.[a-zA-Z0-9_\-]{22}\.[a-zA-Z0-9_\-]{43}",                             "high"),
    ("Mailgun API Key",            r"key-[a-zA-Z0-9]{32}",                                                     "high"),
    ("Mailchimp API Key",          r"[a-f0-9]{32}-us[0-9]{1,2}",                                               "medium"),

    // Azure / Microsoft
    ("Azure Storage Key",          r"AccountKey=[A-Za-z0-9+/]{88}==",                                         "critical"),
    ("Azure SAS Token",            r"sv=[0-9]{4}-[0-9]{2}-[0-9]{2}&",                                          "high"),

    // Private keys
    ("RSA Private Key",            r"-----BEGIN RSA PRIVATE KEY-----",                                         "critical"),
    ("EC Private Key",             r"-----BEGIN EC PRIVATE KEY-----",                                          "critical"),
    ("OpenSSH Private Key",        r"-----BEGIN OPENSSH PRIVATE KEY-----",                                     "critical"),
    ("Generic Private Key",        r"-----BEGIN PRIVATE KEY-----",                                             "critical"),

    // JWT (loose — only flag when in suspicious context)
    ("JWT Token",                  r"eyJ[a-zA-Z0-9_\-]{20,}\.[a-zA-Z0-9_\-]{20,}\.[a-zA-Z0-9_\-]{20,}",     "medium"),

    // Heroku
    ("Heroku API Key",             r"(?i)heroku.{0,30}[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", "high"),

    // NPM tokens
    ("NPM Automation Token",       r"npm_[a-zA-Z0-9]{36}",                                                     "high"),
    ("NPM Legacy Token",           r"[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}",          "medium"),

    // Telegram
    ("Telegram Bot Token",         r"[0-9]{7,12}:[a-zA-Z0-9_\-]{35}",                                         "high"),

    // Generic high-entropy env var assignments
    ("Generic API Key Env",        r#"(?i)(api_key|apikey|api_secret|access_token|secret_key|auth_token)\s*[=:]\s*["']?[a-zA-Z0-9_\-]{32,}["']?"#, "medium"),
    ("Generic Password Env",       r#"(?i)(password|passwd|pwd)\s*[=:]\s*["'][^"']{8,}["']"#,                 "medium"),
];

// ─── Finding struct ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretFinding {
    pub id:            String,
    pub repo:          String,
    pub repo_url:      String,
    pub owner:         String,
    pub commit_sha:    String,
    pub commit_url:    String,
    pub file_path:     String,
    pub line_number:   Option<usize>,
    pub secret_type:   String,
    pub severity:      String,
    /// Masked value — never the full secret.  e.g. "AKIA****ZXYZ"
    pub preview:       String,
    /// One line of surrounding context with the secret replaced by [REDACTED]
    pub context_line:  String,
    pub detected_at:   String,
    /// Pre-filled GitHub "new issue" URL so the researcher can notify the owner
    pub disclosure_url: String,
    /// True if this finding appeared in a new scan cycle (not seen before)
    pub is_new:        bool,
}

// ─── Scanner ──────────────────────────────────────────────────────────────────

pub struct SecretScanner {
    http:    reqwest::Client,
    buf:     FindingsBuf,
    /// seen_ids prevents duplicates across poll cycles
    seen:    Arc<RwLock<HashMap<String, ()>>>,
    regexes: Vec<(String, Regex, String)>,
}

impl SecretScanner {
    pub fn new(http: reqwest::Client, buf: FindingsBuf) -> anyhow::Result<Self> {
        let regexes = PATTERNS
            .iter()
            .map(|(label, pat, sev)| {
                Regex::new(pat).map(|re| (label.to_string(), re, sev.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            http,
            buf,
            seen: Arc::new(RwLock::new(HashMap::new())),
            regexes,
        })
    }

    /// Run forever — poll GitHub Events and scan commits every 30 seconds.
    pub async fn run_forever(&self) {
        info!("Secret scanner started — polling GitHub Events API every 30s");
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            if let Err(e) = self.poll_once().await {
                warn!("Secret scanner poll error: {e}");
            }
        }
    }

    async fn poll_once(&self) -> anyhow::Result<()> {
        let events: serde_json::Value = self
            .http
            .get("https://api.github.com/events")
            .query(&[("per_page", "30")])
            .header("Accept", "application/vnd.github+json")
            .send()
            .await?
            .json()
            .await?;

        let arr = match events.as_array() {
            Some(a) => a.clone(),
            None => return Ok(()),
        };

        // Only examine PushEvents
        let push_events: Vec<&serde_json::Value> = arr
            .iter()
            .filter(|e| e["type"].as_str() == Some("PushEvent"))
            .collect();

        // Limit to 8 per cycle so we don't hammer the API
        for event in push_events.iter().take(8) {
            let repo_name = event["repo"]["name"].as_str().unwrap_or("").to_string();
            let commits = match event["payload"]["commits"].as_array() {
                Some(c) => c.clone(),
                None => continue,
            };
            for commit in commits.iter().take(3) {
                let sha = commit["sha"].as_str().unwrap_or("").to_string();
                if sha.is_empty() { continue; }
                if let Err(e) = self.scan_commit(&repo_name, &sha).await {
                    warn!("scan_commit {repo_name}@{sha}: {e}");
                }
                // Small sleep so we don't get rate-limited
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
        }
        Ok(())
    }

    async fn scan_commit(&self, repo: &str, sha: &str) -> anyhow::Result<()> {
        let url = format!("https://api.github.com/repos/{repo}/commits/{sha}");
        let resp = self.http
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await?;
        if !resp.status().is_success() {
            return Ok(()); // private repo, rate-limit, etc.
        }
        let data: serde_json::Value = resp.json().await?;

        let files = match data["files"].as_array() {
            Some(f) => f.clone(),
            None => return Ok(()),
        };

        let owner = repo.split('/').next().unwrap_or("unknown").to_string();
        let commit_url = format!("https://github.com/{repo}/commit/{sha}");

        for file in &files {
            let file_path = file["filename"].as_str().unwrap_or("").to_string();
            let patch = match file["patch"].as_str() {
                Some(p) => p.to_string(),
                None => continue,
            };

            // Skip known false-positive files
            if is_ignored_file(&file_path) { continue; }

            self.scan_patch(
                repo, &owner, sha, &commit_url, &file_path, &patch,
            )
            .await;
        }
        Ok(())
    }

    async fn scan_patch(
        &self,
        repo: &str,
        owner: &str,
        sha: &str,
        commit_url: &str,
        file_path: &str,
        patch: &str,
    ) {
        let mut line_no: usize = 0;
        for raw_line in patch.lines() {
            // Only scan added lines (starts with '+' but not '+++')
            if raw_line.starts_with("+++") {
                continue;
            }
            if raw_line.starts_with("@@") {
                // Extract new-file start line from @@ -a,b +c,d @@
                if let Some(n) = parse_hunk_line(raw_line) {
                    line_no = n;
                }
                continue;
            }
            if raw_line.starts_with('+') {
                line_no += 1;
                let line = &raw_line[1..]; // strip leading '+'
                for (label, re, sev) in &self.regexes {
                    if let Some(m) = re.find(line) {
                        let matched = m.as_str();
                        // Build a stable finding ID
                        let finding_id = format!("{repo}:{sha}:{file_path}:{line_no}:{label}");

                        {
                            let s = self.seen.read().await;
                            if s.contains_key(&finding_id) { continue; }
                        }
                        // Mark as seen
                        self.seen.write().await.insert(finding_id.clone(), ());

                        let preview   = mask_secret(matched);
                        let ctx_line  = line.replacen(matched, "[REDACTED]", 1)
                                          .chars().take(120).collect::<String>();

                        let disclosure_body = format!(
                            "Hello! I found a potentially exposed credential in this repository.\n\n\
                            **Type:** {label}\n\
                            **File:** `{file_path}` (line ~{line_no})\n\
                            **Commit:** {commit_url}\n\n\
                            Please rotate this credential immediately.\n\n\
                            *This report was generated automatically by repo-radar. \
                            The actual secret value was not stored.*"
                        );
                        let disclosure_url = format!(
                            "https://github.com/{}/issues/new?title={}&body={}",
                            repo,
                            urlencoding_simple("Security: Exposed API Key/Secret"),
                            urlencoding_simple(&disclosure_body),
                        );

                        let finding = SecretFinding {
                            id: finding_id,
                            repo: repo.to_string(),
                            repo_url: format!("https://github.com/{repo}"),
                            owner: owner.to_string(),
                            commit_sha: sha[..8.min(sha.len())].to_string(),
                            commit_url: commit_url.to_string(),
                            file_path: file_path.to_string(),
                            line_number: Some(line_no),
                            secret_type: label.clone(),
                            severity: sev.clone(),
                            preview,
                            context_line: ctx_line,
                            detected_at: Utc::now().to_rfc3339(),
                            disclosure_url,
                            is_new: true,
                        };

                        info!(
                            secret_type = %label,
                            repo = %repo,
                            file = %file_path,
                            line = line_no,
                            severity = %sev,
                            "🔑 Secret detected"
                        );

                        let mut buf = self.buf.write().await;
                        buf.push_front(finding);
                        if buf.len() > MAX_FINDINGS {
                            buf.pop_back();
                        }
                    }
                }
            } else if !raw_line.starts_with('-') {
                line_no += 1;
            }
        }
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Mask a secret: keep first 4 chars + **** + last 4 chars.
fn mask_secret(s: &str) -> String {
    if s.len() <= 8 {
        return "*".repeat(s.len());
    }
    format!("{}****{}", &s[..4], &s[s.len() - 4..])
}

/// Parse the new-file starting line number from a unified diff hunk header.
/// Example:  "@@ -10,7 +23,8 @@ fn foo()" → returns 23
fn parse_hunk_line(hd: &str) -> Option<usize> {
    // find the `+N` part
    let plus_idx = hd.find("++")?; // skip '+++'
    let after = &hd[plus_idx + 1..];
    let plus = after.find('+')?;
    let numpart = &after[plus + 1..];
    let end = numpart.find(|c: char| !c.is_ascii_digit()).unwrap_or(numpart.len());
    numpart[..end].parse().ok()
}

/// Minimal percent-encoding for use inside a URL query string.
fn urlencoding_simple(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'
            | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}

/// Files that are almost always false positives.
fn is_ignored_file(path: &str) -> bool {
    let lp = path.to_lowercase();
    lp.ends_with(".lock")
        || lp.ends_with(".sum")
        || lp.ends_with(".min.js")
        || lp.ends_with(".map")
        || lp.contains("node_modules")
        || lp.contains("vendor/")
        || lp.contains("test/fixtures")
        || lp.contains("__snapshots__")
        || lp.ends_with(".md")
        || lp.ends_with(".txt")
}
