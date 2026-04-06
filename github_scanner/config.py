"""
config.py — all tunable settings in one place.

Set GITHUB_TOKEN in your environment for 5000 req/hr (vs 60 unauthenticated).
Set REDIS_URL if Redis is on a non-default host/port.
"""
import os

# ── GitHub ─────────────────────────────────────────────────────────────────────
GITHUB_TOKEN   = os.getenv("GITHUB_TOKEN", "")
GITHUB_API     = "https://api.github.com"
USER_AGENT     = "repo-radar-scanner/1.0 (github.com/lemwaiping123-eng/repo-radar)"

# How many public events to fetch per poll cycle (max 100)
EVENTS_PER_PAGE = 30
# Commits to scan per PushEvent
MAX_COMMITS_PER_EVENT = 3
# Events to process per poll cycle
MAX_EVENTS_PER_CYCLE = 10
# Seconds between polls (GitHub Events API updates every 60s max anyway)
POLL_INTERVAL_SECS = 30

# ── Redis ──────────────────────────────────────────────────────────────────────
REDIS_URL          = os.getenv("REDIS_URL", "redis://localhost:6379")
REDIS_SECRETS_KEY  = "repo-radar:secrets"   # list of JSON findings
REDIS_SEEN_PREFIX  = "repo-radar:seen:"     # dedup set
REDIS_SECRETS_TTL  = 60 * 60 * 24          # 24 h — keep findings one day
REDIS_SEEN_TTL     = 60 * 60 * 6           # 6 h  — dedup window
MAX_FINDINGS       = 500                    # ring-buffer cap in Redis

# ── Shannon entropy ────────────────────────────────────────────────────────────
# Strings with entropy >= this value are likely random keys,
# not coincidental pattern matches.
HIGH_ENTROPY_THRESHOLD = 3.5   # bits per character
# Minimum token length to calculate entropy on (skip short words)
MIN_ENTROPY_LENGTH = 20

# ── Secret patterns ────────────────────────────────────────────────────────────
# (label, regex_pattern, severity)
# severity: "critical" | "high" | "medium" | "low"
SECRET_PATTERNS = [
    # AWS
    ("AWS Access Key ID",         r"AKIA[0-9A-Z]{16}",                                                "critical"),
    ("AWS Secret Access Key",     r"(?i)aws.{0,30}secret.{0,30}[A-Za-z0-9/+=]{40}",                  "critical"),

    # GitHub
    ("GitHub Personal Token",     r"ghp_[a-zA-Z0-9]{36}",                                             "critical"),
    ("GitHub Fine-grained Token", r"github_pat_[a-zA-Z0-9_]{82}",                                     "critical"),
    ("GitHub App Install Token",  r"ghs_[a-zA-Z0-9]{36}",                                             "high"),
    ("GitHub OAuth Token",        r"gho_[a-zA-Z0-9]{36}",                                             "high"),

    # AI
    ("OpenAI API Key",            r"sk-[a-zA-Z0-9]{48}",                                              "critical"),
    ("Anthropic API Key",         r"sk-ant-api\d{2}-[a-zA-Z0-9_-]{93}",                               "critical"),
    ("Anthropic Key (short)",     r"sk-ant-[a-zA-Z0-9_-]{40,}",                                       "critical"),
    ("Cohere API Key",            r"cohere[_\-\s]+[a-zA-Z0-9]{40}",                                   "high"),

    # Payments
    ("Stripe Live Secret Key",    r"sk_live_[a-zA-Z0-9]{24,}",                                        "critical"),
    ("Stripe Test Secret Key",    r"sk_test_[a-zA-Z0-9]{24,}",                                        "medium"),
    ("Stripe Restricted Key",     r"rk_live_[a-zA-Z0-9]{24,}",                                        "high"),

    # Google
    ("Google API Key",            r"AIza[0-9A-Za-z\-_]{35}",                                          "high"),
    ("Google OAuth Client ID",    r"[0-9]+-[a-z0-9]+\.apps\.googleusercontent\.com",                  "medium"),

    # Slack
    ("Slack Bot Token",           r"xoxb-[0-9]{10,}-[0-9]{10,}-[a-zA-Z0-9]{24}",                     "high"),
    ("Slack User Token",          r"xoxp-[0-9]{10,}-[0-9]{10,}-[0-9]{10,}-[a-zA-Z0-9]{32}",          "high"),
    ("Slack Webhook",             r"hooks\.slack\.com/services/T[a-zA-Z0-9_]{8}/B[a-zA-Z0-9_]{8}/[a-zA-Z0-9_]{24}", "high"),

    # Communication
    ("Twilio Account SID",        r"AC[a-f0-9]{32}",                                                   "medium"),
    ("Twilio Auth Token",         r"(?i)twilio.{0,30}[a-f0-9]{32}",                                   "high"),
    ("Discord Bot Token",         r"[MN][a-zA-Z0-9]{23}\.[a-zA-Z0-9\-_]{6}\.[a-zA-Z0-9\-_]{27}",    "high"),
    ("Discord Webhook",           r"discord(?:app)?\.com/api/webhooks/[0-9]{18}/[a-zA-Z0-9_\-]{68}", "high"),
    ("Telegram Bot Token",        r"[0-9]{7,12}:[a-zA-Z0-9_\-]{35}",                                  "high"),

    # Email
    ("SendGrid API Key",          r"SG\.[a-zA-Z0-9_\-]{22}\.[a-zA-Z0-9_\-]{43}",                     "high"),
    ("Mailgun API Key",           r"key-[a-zA-Z0-9]{32}",                                              "high"),
    ("Mailchimp API Key",         r"[a-f0-9]{32}-us[0-9]{1,2}",                                       "medium"),

    # Cloud
    ("Azure Storage Key",         r"AccountKey=[A-Za-z0-9+/]{88}==",                                  "critical"),
    ("Azure SAS Token",           r"sv=[0-9]{4}-[0-9]{2}-[0-9]{2}&",                                  "high"),
    ("Heroku API Key",            r"(?i)heroku.{0,30}[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", "high"),

    # Keys / tokens
    ("RSA Private Key",           r"-----BEGIN RSA PRIVATE KEY-----",                                  "critical"),
    ("EC Private Key",            r"-----BEGIN EC PRIVATE KEY-----",                                   "critical"),
    ("OpenSSH Private Key",       r"-----BEGIN OPENSSH PRIVATE KEY-----",                              "critical"),
    ("Generic Private Key",       r"-----BEGIN PRIVATE KEY-----",                                      "critical"),
    ("JWT Token",                 r"eyJ[a-zA-Z0-9_\-]{20,}\.[a-zA-Z0-9_\-]{20,}\.[a-zA-Z0-9_\-]{20,}", "medium"),
    ("NPM Automation Token",      r"npm_[a-zA-Z0-9]{36}",                                              "high"),

    # Generic env var assignments
    ("Generic API Key Env",       r'(?i)(api_key|apikey|api_secret|access_token|secret_key|auth_token)\s*[=:]\s*["\']?[a-zA-Z0-9_\-]{32,}["\']?', "medium"),
    ("Generic Password Env",      r'(?i)(password|passwd|pwd)\s*[=:]\s*["\'][^"\']{8,}["\']',          "medium"),
]

# Files/paths to skip (noisy, rarely contain real secrets)
IGNORED_EXTENSIONS = {
    ".lock", ".sum", ".mod", ".min.js", ".map", ".snap", ".log",
    ".txt", ".md", ".rst", ".csv", ".json.sample",
}
IGNORED_PATH_FRAGMENTS = {
    "node_modules", "vendor/", "test/fixtures", "testdata/",
    "__pycache__", ".git/", "dist/", "build/",
}
