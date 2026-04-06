# repo-radar 🔭

**Real-time GitHub trend detector powered by Rust + Redis**

Get notified within minutes when a repo starts spiking — on Discord or Telegram.
Never miss the next viral project.

[![CI](https://github.com/lemwaiping123-eng/repo-radar/actions/workflows/ci.yml/badge.svg)](https://github.com/lemwaiping123-eng/repo-radar/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.81+-orange?logo=rust)](https://rust-lang.org)

---

## Why?

When [claude-code](https://github.com/anthropics/claude-code) leaked it hit 171k stars in days.
Developers who found it **first** got early adopter advantages — content, integrations, mindshare.

**repo-radar** watches GitHub trending + Hacker News 24/7 and pings you the moment a repo starts its hockey-stick growth.

---

## Features

- 🔭 **Dual-source monitoring** — GitHub Search API + HN Algolia `Show HN` feed
- ⚡ **Spike detection** — configurable star-velocity threshold (default: +500 stars/24h)
- 🔄 **Redis deduplication** — no duplicate alerts within a configurable window
- 📢 **Discord + Telegram** notifications with rich formatting
- 🐳 **Docker Compose** — one command to run Redis + detector
- 🔁 **CI/CD** — GitHub Actions with multi-Rust-version matrix + security audit
- 🔌 **Redis pub/sub** — subscribe to `repo-radar:events` from your own code

---

## Quick start

### Docker (recommended)

```bash
git clone https://github.com/lemwaiping123-eng/repo-radar
cd repo-radar

cp .env.example .env
# Edit .env: add GITHUB_TOKEN, DISCORD_WEBHOOK_URL or TELEGRAM_BOT_TOKEN

docker compose up -d
docker compose logs -f repo-radar
```

### Native Rust build

```bash
# Prerequisites: Rust 1.81+, a running Redis instance
cargo build --release

# Watch mode — polls GitHub every 5 min, HN every 3 min
REDIS_URL=redis://127.0.0.1:6379 ./target/release/repo-radar watch

# One-shot repo check
./target/release/repo-radar check rust-lang/rust

# Show recent alerts
./target/release/repo-radar status
```

---

## Configuration

All settings via environment variables or `.env` file:

| Variable | Default | Description |
|----------|---------|-------------|
| `REDIS_URL` | `redis://127.0.0.1:6379` | Redis connection URL |
| `GITHUB_TOKEN` | _(optional)_ | GitHub PAT — increases API rate limit 10× |
| `DISCORD_WEBHOOK_URL` | _(optional)_ | Discord webhook for alerts |
| `TELEGRAM_BOT_TOKEN` | _(optional)_ | Telegram bot token |
| `TELEGRAM_CHAT_ID` | _(optional)_ | Telegram chat/channel ID |
| `SPIKE_THRESHOLD` | `500` | Stars gained in 24h to trigger alert |
| `MIN_STARS` | `50` | Minimum total stars to consider |
| `POLL_INTERVAL_SECS` | `300` | GitHub polling interval (seconds) |
| `DEDUP_TTL_HOURS` | `6` | Hours before re-alerting same repo |

---

## Architecture

```
GitHub Search API  ──┐
                      ├──► Detector (Rust)
HN Algolia API    ──┘         │
                              ├──► Redis LIST  (alert history)
                              ├──► Redis SETEX (dedup keys)
                              ├──► Redis PUB   (repo-radar:events)
                              ├──► Discord webhook
                              └──► Telegram Bot API
```

**Spike score** = `stars_24h + (growth_factor × 100)`  
where `growth_factor = stars_24h / (total_stars − stars_24h)`

Higher growth_factor means the spike is proportionally large relative to the repo's history — a repo going 50→500 stars ranks above one going 50000→50500.

---

## Subscribe via Redis pub/sub

```bash
redis-cli subscribe repo-radar:events
```

Each message is a JSON-serialized `Alert` object — integrate with your own tools.

---

## CLI reference

```
repo-radar watch          Watch GitHub + HN continuously
repo-radar check <repo>   One-shot spike check, e.g. rust-lang/rust
repo-radar status         Show last 20 alerts from Redis
repo-radar --help         Full help
```

---

## Development

```bash
# Run unit tests (no Redis required)
cargo test --lib

# Run all tests (requires Redis on :6379)
cargo test

# Lint
cargo clippy --all-targets -- -D warnings

# Format
cargo fmt
```

---

## License

MIT © 2025 lemwaiping123-eng
