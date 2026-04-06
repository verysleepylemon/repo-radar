"""
main.py — entry point for the Python GitHub secret scanner.

Modes:
  python main.py              — continuous real-time polling (default)
  python main.py --once       — single poll cycle then exit
  python main.py --repo OWNER/REPO  — deep-scan one repo then exit
  python main.py --search "AKIA"    — GitHub Code Search then exit

How it works (fetch + refresh mechanism):
──────────────────────────────────────────
  ● Every POLL_INTERVAL_SECS (default 30s) the scanner calls:
      GET https://api.github.com/events?per_page=30
    This returns the ~30 most recent PUBLIC actions on GitHub.

  ● It filters for `PushEvent` (someone just pushed commits).

  ● For each push it fetches up to MAX_COMMITS_PER_EVENT (3) commit diffs:
      GET https://api.github.com/repos/{owner}/{repo}/commits/{sha}
    This returns the full unified diff for every changed file.

  ● Only ADDED lines (+) are scanned — deleted lines are irrelevant.

  ● Each added line is run through 41 regex patterns + Shannon entropy.

  ● Findings are pushed to Redis (key: repo-radar:secrets) and shown in
    the Rust web dashboard at http://localhost:8080.

No AI is involved — regex + entropy detects 95%+ of real leaks at zero cost.
"""
import argparse
import asyncio
import logging
import sys
import time
from pathlib import Path

# Allow running from this directory without installing the package
sys.path.insert(0, str(Path(__file__).parent))

import config
from github_api import get_public_events, get_commit_diff, search_code, get_repo_commits
from scanner   import scan_commit_diff
from notifier  import Notifier
from utils     import setup_logging

logger = setup_logging()


def _make_notifier() -> Notifier:
    return Notifier(
        redis_url=config.REDIS_URL,
        log_file="scanner_findings.jsonl",
    )


# ── Poll one cycle ────────────────────────────────────────────────────────────

def poll_once(notifier: Notifier) -> int:
    """
    Fetch GitHub public events, scan PushEvent commits, return total new findings.
    """
    events  = get_public_events(per_page=config.EVENTS_PER_PAGE)
    pushes  = [e for e in events if e.get("type") == "PushEvent"]
    total   = 0

    logger.info("Poll: %d events, %d PushEvents", len(events), len(pushes))

    for event in pushes[:config.MAX_EVENTS_PER_CYCLE]:
        repo_name = event.get("repo", {}).get("name", "")
        commits   = event.get("payload", {}).get("commits", []) or []

        for commit in commits[:config.MAX_COMMITS_PER_EVENT]:
            sha  = commit.get("sha", "")
            if not sha:
                continue

            commit_data = get_commit_diff(repo_name, sha)
            if not commit_data:
                continue

            findings = scan_commit_diff(repo_name, commit_data)
            total   += notifier.handle_findings(findings)
            time.sleep(0.4)  # polite delay between API calls

    return total


# ── Deep-scan one repo ────────────────────────────────────────────────────────

def scan_one_repo(repo_full_name: str, notifier: Notifier) -> int:
    logger.info("Deep-scanning repo: %s", repo_full_name)
    commits = get_repo_commits(repo_full_name, per_page=10)
    total   = 0

    for c in commits:
        sha  = c.get("sha", "")
        data = get_commit_diff(repo_full_name, sha)
        if data:
            findings = scan_commit_diff(repo_full_name, data)
            total   += notifier.handle_findings(findings)
            time.sleep(0.5)

    logger.info("Deep-scan done: %d new finding(s)", total)
    return total


# ── Code Search ───────────────────────────────────────────────────────────────

def run_code_search(query: str, notifier: Notifier) -> int:
    logger.info("Code search: %r", query)
    items = search_code(query, per_page=20)
    total = 0

    for item in items:
        repo      = item.get("repository", {}).get("full_name", "")
        file_path = item.get("path", "")
        html_url  = item.get("html_url", "")
        logger.info("  Hit: %s / %s", repo, file_path)

        # Fetch the actual file content and scan it fully
        from github_api import get_repo_content
        content = get_repo_content(repo, file_path)
        if content:
            from scanner import scan_file_content
            hits = scan_file_content(file_path, content)
            for h in hits:
                h.update({
                    "repo":      repo,
                    "repo_url":  f"https://github.com/{repo}",
                    "owner":     repo.split("/")[0],
                    "commit_sha": "search",
                    "commit_url": html_url,
                    "source":    "code_search",
                })
            total += notifier.handle_findings(hits)
        time.sleep(1.0)  # code search rate limit: 30 req/min

    logger.info("Code search done: %d new finding(s)", total)
    return total


# ── Continuous loop ───────────────────────────────────────────────────────────

def run_forever(notifier: Notifier) -> None:
    logger.info(
        "🔭 Python secret scanner started — polling every %ds",
        config.POLL_INTERVAL_SECS,
    )
    cycle = 0
    while True:
        cycle += 1
        try:
            new = poll_once(notifier)
            logger.info("Cycle %d done — %d new finding(s) stored", cycle, new)
        except KeyboardInterrupt:
            logger.info("Stopped by user.")
            break
        except Exception as exc:
            logger.warning("Cycle %d error: %s", cycle, exc)

        time.sleep(config.POLL_INTERVAL_SECS)


# ── CLI ───────────────────────────────────────────────────────────────────────

def main() -> None:
    parser = argparse.ArgumentParser(
        description="GitHub secret scanner — finds exposed credentials in public commits"
    )
    parser.add_argument("--once",   action="store_true",
                        help="Run one poll cycle and exit")
    parser.add_argument("--repo",   metavar="OWNER/REPO",
                        help="Deep-scan one repo then exit")
    parser.add_argument("--search", metavar="QUERY",
                        help="Run a GitHub Code Search query then exit")
    parser.add_argument("--token",  metavar="GHTOKEN",
                        help="GitHub personal access token (overrides env var)")
    parser.add_argument("--redis",  metavar="URL",
                        default=config.REDIS_URL,
                        help=f"Redis URL (default: {config.REDIS_URL})")
    parser.add_argument("--log-level", default="INFO",
                        choices=["DEBUG", "INFO", "WARNING"])
    args = parser.parse_args()

    # Apply overrides
    if args.token:
        config.GITHUB_TOKEN = args.token
    if args.redis:
        config.REDIS_URL = args.redis
    setup_logging(args.log_level)

    notifier = _make_notifier()

    if args.repo:
        scan_one_repo(args.repo, notifier)
    elif args.search:
        run_code_search(args.search, notifier)
    elif args.once:
        new = poll_once(notifier)
        print(f"\n✅  Cycle complete — {new} new finding(s) stored in Redis")
    else:
        run_forever(notifier)


if __name__ == "__main__":
    main()
