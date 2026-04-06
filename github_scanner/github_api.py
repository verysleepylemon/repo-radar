"""
github_api.py — all GitHub API interactions.

Functions:
  get_public_events()        — latest public GitHub events (real-time stream)
  get_repo_commits()         — commit list for a repo
  get_commit_diff()          — full diff (file patches) for one commit
  get_repo_content()         — raw content of a single file
  search_code()              — GitHub Code Search (keyword + language)
  get_user_repos()           — public repos for a user/org

Rate limits (without token):  60 req/hr
Rate limits (with token):   5000 req/hr
"""
import time
import logging
from typing import Optional

import requests

import config

logger = logging.getLogger("github_scanner.api")


def _headers() -> dict:
    h = {
        "Accept": "application/vnd.github+json",
        "User-Agent": config.USER_AGENT,
        "X-GitHub-Api-Version": "2022-11-28",
    }
    if config.GITHUB_TOKEN:
        h["Authorization"] = f"Bearer {config.GITHUB_TOKEN}"
    return h


def _get(url: str, params: Optional[dict] = None, retries: int = 2) -> Optional[dict | list]:
    """
    Simple GET with retry and rate-limit awareness.
    Returns parsed JSON or None on failure.
    """
    for attempt in range(retries + 1):
        try:
            resp = requests.get(url, headers=_headers(), params=params, timeout=15)
        except requests.RequestException as exc:
            logger.warning("Request failed (%s): %s", url, exc)
            if attempt < retries:
                time.sleep(2 ** attempt)
            continue

        # Rate limit handling
        if resp.status_code == 403 and "rate limit" in resp.text.lower():
            reset = int(resp.headers.get("X-RateLimit-Reset", time.time() + 60))
            wait = max(5, reset - int(time.time()))
            logger.warning("Rate limited — sleeping %ds", wait)
            time.sleep(min(wait, 60))
            continue

        if resp.status_code == 422:
            # Search API quota / validation error — not retryable
            logger.debug("422 from %s — skipping", url)
            return None

        if not resp.ok:
            logger.debug("HTTP %d from %s", resp.status_code, url)
            return None

        try:
            return resp.json()
        except ValueError:
            logger.warning("Invalid JSON from %s", url)
            return None

    return None


# ── Public Events ──────────────────────────────────────────────────────────────

def get_public_events(per_page: int = 30) -> list[dict]:
    """
    Fetch the latest public GitHub events.

    GitHub updates this endpoint roughly every 60 seconds.
    Returns a list of event objects (may include PushEvent, CreateEvent, etc.).
    No authentication needed — but authenticated requests get higher rate limits.
    """
    url = f"{config.GITHUB_API}/events"
    result = _get(url, params={"per_page": per_page})
    return result if isinstance(result, list) else []


# ── Commits ────────────────────────────────────────────────────────────────────

def get_repo_commits(repo_full_name: str, per_page: int = 10) -> list[dict]:
    """
    Return the latest commits for a repo (owner/repo format).
    Each item has: sha, commit.message, author, html_url.
    """
    url = f"{config.GITHUB_API}/repos/{repo_full_name}/commits"
    result = _get(url, params={"per_page": per_page})
    return result if isinstance(result, list) else []


def get_commit_diff(repo_full_name: str, commit_sha: str) -> Optional[dict]:
    """
    Fetch full commit data including `files` — each file has a `patch` field
    (unified diff text) showing exactly what changed.

    This is the primary data source for secret scanning:
      commit["files"][n]["patch"]  — the diff for file n
      commit["files"][n]["filename"] — the file path
    """
    url = f"{config.GITHUB_API}/repos/{repo_full_name}/commits/{commit_sha}"
    return _get(url)


# ── File content ───────────────────────────────────────────────────────────────

def get_repo_content(repo_full_name: str, file_path: str,
                     ref: str = "HEAD") -> Optional[str]:
    """
    Return the raw (decoded) text content of a single file.
    Returns None if the file is binary or not found.

    Useful for deep-scanning specific suspicious files found via Code Search.
    """
    url = f"{config.GITHUB_API}/repos/{repo_full_name}/contents/{file_path}"
    data = _get(url, params={"ref": ref})
    if not data or data.get("encoding") != "base64":
        return None
    import base64
    try:
        return base64.b64decode(data["content"]).decode("utf-8", errors="replace")
    except Exception:
        return None


# ── Code Search ───────────────────────────────────────────────────────────────

def search_code(query: str, language: Optional[str] = None,
                per_page: int = 10) -> list[dict]:
    """
    GitHub Code Search — find files matching 'query' across all public repos.

    ⚠️  Requires authentication (GITHUB_TOKEN).  Returns up to 1000 results
    with 30 req/min rate limit (even authenticated).

    Example queries:
      "AKIA language:python"     — AWS keys in Python files
      "sk-live_ extension:env"   — Stripe live keys in .env files
      "BEGIN PRIVATE KEY"        — private key blocks anywhere
    """
    if language:
        query = f"{query} language:{language}"
    url = f"{config.GITHUB_API}/search/code"
    result = _get(url, params={"q": query, "per_page": per_page})
    if isinstance(result, dict) and "items" in result:
        return result["items"]
    return []


# ── User / Org repos ──────────────────────────────────────────────────────────

def get_user_repos(username: str, per_page: int = 30) -> list[dict]:
    """
    Return public repos for a GitHub user or organization.
    Useful for targeted scanning of a specific account.
    """
    url = f"{config.GITHUB_API}/users/{username}/repos"
    result = _get(url, params={"per_page": per_page, "sort": "pushed"})
    return result if isinstance(result, list) else []
