# repo-radar 🔭

> **The fastest way to know when the internet discovers something before everyone else does.**

Real-time GitHub intelligence platform built in Rust — monitors trending repos, detects leaked source code, scans public commits for exposed secrets, and alerts you within minutes.

[![CI](https://github.com/lemwaiping123-eng/repo-radar/actions/workflows/ci.yml/badge.svg)](https://github.com/lemwaiping123-eng/repo-radar/actions)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.81+-orange?logo=rust)](https://rust-lang.org)
[![Redis](https://img.shields.io/badge/Redis-7+-red?logo=redis)](https://redis.io)

---

## Why repo-radar?

When **Claude Code** leaked in March 2026, it went from 0 → 171k stars in days.
People who found it first got a head start on unreleased features, model names, and architecture.

**repo-radar** is the system that would have found that leak for you — automatically.

It does five things at once, continuously:

| Layer | What it watches |
|---|---|
| 🔭 **Trend radar** | GitHub Search API + Hacker News — spikes in star velocity |
| 🔑 **Secret scanner** | Every public PushEvent diff — 35+ credential patterns |
| 📦 **NPM monitor** | AI/ML packages — abnormal bundle sizes that hint at embedded source maps |
| 💥 **Leak tracker** | Confirmed and suspected source-code leaks with reproduction steps |
| 📡 **Social pulse** | Reddit (8 subs) + Dev.to — what the community is talking about right now |

---

## Quickstart

```bash
# 1. Clone
git clone https://github.com/lemwaiping123-eng/repo-radar
cd repo-radar

# 2. Start Redis
redis-server &    # or: docker run -d -p 6379:6379 redis:7-alpine

# 3. Build (requires Rust 1.81+)
cargo build --release

# 4. Run the full stack — web dashboard + all monitors
./target/release/repo-radar serve --port 8080
```

Open **http://localhost:8080** — the dashboard loads immediately.

For notifications, add to your `.env`:

```env
DISCORD_WEBHOOK_URL=https://discord.com/api/webhooks/...
TELEGRAM_BOT_TOKEN=...
TELEGRAM_CHAT_ID=...
GITHUB_TOKEN=ghp_...          # optional, 10x rate limit
OPENROUTER_API_KEY=sk-or-...  # optional, enables auto Rust port generation
```

---

## Dashboard endpoints

| Endpoint | Description |
|---|---|
| `GET /api/leaks` | Confirmed and suspected source leaks, classified by severity |
| `GET /api/alerts` | Trending repo alerts from GitHub + HN + RSS |
| `GET /api/secrets` | Exposed credentials found in public commits (masked) |
| `GET /api/vip` | VIP keyword hits — "leaked", "unreleased", "banned", "censored" |
| `GET /api/social` | Raw social pulse — 8 Reddit subs + Dev.to, no filter |
| `GET /api/npm` | NPM packages with abnormal bundle sizes |
| `GET /api/trends` | GitHub trending repos |

All responses are JSON. All data auto-purges after **3 days**.

---

## Secret scanner

Scans every public `PushEvent` on GitHub Events API every 30 seconds.

**Pattern library (35+ rules):**
AWS · GitHub PAT · OpenAI · Anthropic · Stripe · Google · Slack · Twilio · Discord · SendGrid · Mailgun · Azure Storage · Private keys (RSA/EC/OpenSSH) · JWT · Heroku · NPM tokens · Telegram bot tokens · Generic `API_KEY=` env assignments

**Performance optimizations** (borrowed from [noseyparker](https://github.com/praetorian-inc/noseyparker) and [rusty-hog](https://github.com/newrelic/rusty-hog)):

- **Aho-Corasick pre-filter** — literal keyword automaton skips ~95% of lines before regex evaluation
- **False-positive allowlist** — suppresses `example`, `changeme`, `fake`, `your_api_key` matches
- **File allowlist** — skips `node_modules/`, `vendor/`, `*.lock`, `dist/`, test fixtures, docs

Secrets are **masked before storage** (`AKIA****ZXYZ`). Full values are never saved.

---

## Auto-replicator

When a critical/confirmed leak is detected, repo-radar automatically:

1. `git clone --depth 1` the leaked repository
2. Summarises the source (up to 12 KB of key files)
3. Calls OpenRouter's free Gemini model to generate a **Rust port scaffold**
4. Saves everything to `replications/<leak-id>/`

```
replications/
└── claude-code-2026/
    ├── leak_meta.json              # structured metadata
    ├── source/                     # git clone of the leaked repo
    └── typescript_to_rust_port.rs  # LLM-generated port scaffold
```

Set `OPENROUTER_API_KEY` (free at [openrouter.ai](https://openrouter.ai)) to activate.
Override the model with `REPLICATOR_MODEL` (default: `google/gemini-2.5-pro-exp-03-25:free`).

---

## Architecture

```
GitHub Events API --+
GitHub Search API   +---> Detector (Rust/Tokio)
HN + RSS feeds  ---+              |
                                  +---> Redis LIST    (3-day rolling window)
NPM Registry --------------------> --- NPM Monitor
                                  |
                                  +---> Web dashboard  (Axum, :8080)
                                  +---> Discord / Telegram webhooks
                                  +---> replications/  (auto-replicator)

Secret scanner -----------------> GitHub PushEvents -> commit diffs -> 35+ regex patterns
                                  (Aho-Corasick pre-filter + false-positive allowlist)
```

**Spike score** = `stars_24h + (growth_factor x 100)`
`growth_factor = stars_24h / (total_stars - stars_24h)` — small repos that double overnight rank ahead of large repos that add the same raw count.

---

## Real-world catch: Claude Code (March 2026)

```
Package  : @anthropic-ai/claude-code
Vector   : Bun bundler generates .map files by default
           .npmignore did not exclude *.map
           sourcesContent field embeds full TypeScript source
Exposed  : BUDDY (Tamagotchi AI), KAIROS (always-on mode),
           ULTRAPLAN, Coordinator/Swarm multi-agent,
           models Capybara / Opus 4.7 / Sonnet 4.8
```

repo-radar's NPM monitor would have flagged the 42 MB bundle and the `.map` files within minutes of publish.

---

## Configuration

| Variable | Default | Description |
|---|---|---|
| `REDIS_URL` | `redis://127.0.0.1:6379` | Redis connection |
| `GITHUB_TOKEN` | _(optional)_ | PAT — raises rate limit 60 -> 5000 req/hr |
| `DISCORD_WEBHOOK_URL` | _(optional)_ | Alert notifications |
| `TELEGRAM_BOT_TOKEN` | _(optional)_ | Telegram alerts |
| `TELEGRAM_CHAT_ID` | _(optional)_ | Telegram target |
| `SPIKE_THRESHOLD` | `500` | Stars gained in 24h to alert |
| `MIN_STARS` | `50` | Minimum total stars to track |
| `POLL_INTERVAL_SECS` | `300` | GitHub polling interval |
| `OPENROUTER_API_KEY` | _(optional)_ | Enables LLM port generation |
| `REPLICATOR_MODEL` | `google/gemini-2.5-pro-exp-03-25:free` | OpenRouter model |

---

## Development

```bash
cargo test --lib           # unit tests (no Redis needed)
cargo test                 # all tests (needs Redis on :6379)
cargo clippy -- -D warnings
cargo fmt
```

---

## Contributing

Issues and PRs welcome. The codebase is structured as:

```
src/
├── main.rs           # CLI entry point, orchestration
├── web.rs            # Axum web server, all API handlers
├── detector.rs       # spike detection, Alert struct
├── secret_scanner.rs # GitHub Events -> credential scanner
├── replicator.rs     # auto-clone + LLM port generation
├── redis_store.rs    # persistence, 3-day purge
├── config.rs         # env-var config
├── sources/          # data sources (GitHub, HN, RSS, trends)
└── notifiers/        # Discord, Telegram
```

---

## License

MIT (c) 2026 [lemwaiping123-eng](https://github.com/lemwaiping123-eng)
