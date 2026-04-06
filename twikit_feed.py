#!/usr/bin/env python3
"""
twikit_feed.py - Twitter/X scraper sidecar for repo-radar.

Uses the twikit library (no API key required) to search for tech and
security-relevant tweets and writes them to twikit_cache.json so the
Rust service can read them without any Twitter API credentials.

Usage:
    pip install twikit
    python twikit_feed.py           # run once then exit
    python twikit_feed.py --loop    # poll every 15 minutes

Environment variables (required on first run):
    TWITTER_USERNAME  - your Twitter/X username or email
    TWITTER_EMAIL     - account email address
    TWITTER_PASSWORD  - account password

Credentials are persisted in twikit_cookies.json after the first login.
"""

import asyncio
import json
import os
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

try:
    from twikit import Client
except ImportError:
    print("twikit not installed. Run: pip install twikit", file=sys.stderr)
    sys.exit(1)

CACHE_FILE = Path("twikit_cache.json")
COOKIES_FILE = Path("twikit_cookies.json")

SEARCH_QUERIES = [
    # Tech / viral
    "programming lang:en",
    "open source security lang:en",
    "leaked source code lang:en",
    # High-signal dev chatter
    "github stars lang:en",
    "AI tool developer lang:en",
]

MIN_LIKES = 20


async def login(client: Client) -> None:
    """Log in using saved cookies or fresh credentials."""
    if COOKIES_FILE.exists():
        client.load_cookies(str(COOKIES_FILE))
        print("[twikit] Loaded saved cookies")
        return

    username = os.environ.get("TWITTER_USERNAME", "")
    email = os.environ.get("TWITTER_EMAIL", "")
    password = os.environ.get("TWITTER_PASSWORD", "")

    if not (username and email and password):
        print(
            "Set TWITTER_USERNAME, TWITTER_EMAIL and TWITTER_PASSWORD "
            "for the first login.",
            file=sys.stderr,
        )
        sys.exit(1)

    await client.login(auth_info_1=username, auth_info_2=email, password=password)
    client.save_cookies(str(COOKIES_FILE))
    print("[twikit] Logged in and saved cookies")


def tweet_to_dict(tweet) -> dict:
    """Normalise a twikit Tweet object to the JSON shape Rust expects."""
    metrics = getattr(tweet, "public_metrics", None)
    if metrics is None:
        # twikit exposes counts directly on the object
        metrics_dict = {
            "retweet_count": getattr(tweet, "retweet_count", 0) or 0,
            "like_count": getattr(tweet, "favorite_count", 0) or 0,
            "reply_count": getattr(tweet, "reply_count", 0) or 0,
            "quote_count": getattr(tweet, "quote_count", 0) or 0,
        }
    else:
        metrics_dict = {
            "retweet_count": metrics.retweet_count or 0,
            "like_count": metrics.like_count or 0,
            "reply_count": metrics.reply_count or 0,
            "quote_count": metrics.quote_count or 0,
        }

    user = getattr(tweet, "user", None)
    username = getattr(user, "screen_name", None) if user else None
    avatar_url = getattr(user, "profile_image_url_https", None) if user else None

    return {
        "id": str(tweet.id),
        "text": tweet.text,
        "public_metrics": metrics_dict,
        "author_id": str(user.id) if user else None,
        "username": username,
        "avatar_url": avatar_url,
    }


async def collect_tweets(client: Client) -> list[dict]:
    """Search all queries and return deduplicated, engagement-filtered tweets."""
    seen: set[str] = set()
    results: list[dict] = []

    for query in SEARCH_QUERIES:
        try:
            tweets = await client.search_tweet(query, product="Latest", count=20)
            for tweet in tweets:
                tid = str(tweet.id)
                if tid in seen:
                    continue
                seen.add(tid)
                d = tweet_to_dict(tweet)
                likes = d["public_metrics"]["like_count"]
                if likes >= MIN_LIKES:
                    results.append(d)
        except Exception as exc:  # noqa: BLE001
            print(f"[twikit] Query '{query}' failed: {exc}", file=sys.stderr)
        await asyncio.sleep(2)  # be polite between queries

    # Sort by engagement score (mirrors Rust logic)
    def engagement(t: dict) -> int:
        m = t["public_metrics"]
        return m["retweet_count"] * 5 + m["like_count"] + m["reply_count"] * 2 + m["quote_count"] * 3

    results.sort(key=engagement, reverse=True)
    return results


async def run_once() -> None:
    client = Client(language="en-US")
    await login(client)
    print("[twikit] Collecting tweets...")
    tweets = await collect_tweets(client)
    with CACHE_FILE.open("w", encoding="utf-8") as fh:
        json.dump(tweets, fh, ensure_ascii=False, indent=2)
    ts = datetime.now(tz=timezone.utc).isoformat()
    print(f"[twikit] {ts} wrote {len(tweets)} tweets to {CACHE_FILE}")


async def run_loop() -> None:
    interval = 15 * 60  # 15 minutes
    while True:
        try:
            await run_once()
        except Exception as exc:  # noqa: BLE001
            print(f"[twikit] Error: {exc}", file=sys.stderr)
        print(f"[twikit] Sleeping {interval // 60} min...")
        await asyncio.sleep(interval)


if __name__ == "__main__":
    loop_mode = "--loop" in sys.argv
    if loop_mode:
        asyncio.run(run_loop())
    else:
        asyncio.run(run_once())
