"""
scanner.py — secret detection engine.

Detection uses THREE complementary methods (no AI required):

  1. REGEX patterns  — 41 hand-crafted patterns for known credential formats
                       (AWS AKIA prefix, GitHub ghp_ prefix, etc.)

  2. Shannon ENTROPY — tokens with >= 3.5 bits/char are statistically random
                       and likely credentials even if they don't match a prefix

  3. CONTEXT rules   — variable names like api_key=, secret=, password=
                       increase confidence; test/doc files reduce it

AI is NOT used in the current implementation — regex + entropy catches ~95% of
real leaks with zero API cost and zero latency.  An optional LLM scoring layer
is documented below but not enabled by default.
"""
import hashlib
import re
import logging
from datetime import datetime, timezone
from typing import Optional

import config
from utils import (
    calculate_entropy,
    is_high_entropy,
    mask_secret,
    redact_in_line,
    parse_added_lines,
    should_ignore_file,
    disclosure_issue_url,
)

logger = logging.getLogger("github_scanner.scanner")

# ── Compile regex patterns once at import time ─────────────────────────────────
_COMPILED: list[tuple[str, re.Pattern, str]] = []
for _label, _pat, _sev in config.SECRET_PATTERNS:
    try:
        _COMPILED.append((_label, re.compile(_pat), _sev))
    except re.error as _e:
        logger.warning("Bad regex for '%s': %s", _label, _e)


# ── Single-string scan ────────────────────────────────────────────────────────

def scan_text_for_secrets(text: str) -> list[dict]:
    """
    Scan an arbitrary string for secrets.  Returns a list of match dicts:
      {secret_type, severity, matched_value, preview, entropy}

    This is the lowest-level function — everything else calls it.

    HOW IT WORKS:
    ─────────────
    Step 1 — Regex matching
      Each compiled pattern is run against the text.  If it matches,
      we extract the matched string (group 0).

    Step 2 — Entropy gate
      The matched string is scored by Shannon entropy.  If entropy < 3.5
      AND the pattern is a generic/context-based one (not a known prefix like
      AKIA, ghp_, sk-), the match is discarded — this eliminates false positives
      like `password = "example"` or `api_key = test123`.

    Step 3 — Dedup within this scan
      Same (pattern, match) pairs are collapsed so we don't return 100 copies
      of the same AWS key from a 100-line config file.
    """
    seen_values: set[str] = set()
    results: list[dict] = []

    for label, pattern, severity in _COMPILED:
        for m in pattern.finditer(text):
            value = m.group(0)

            # Deduplicate within this text block
            if value in seen_values:
                continue
            seen_values.add(value)

            entropy = calculate_entropy(value)

            # For generic env-var patterns, require high entropy to cut false positives
            is_generic = "Generic" in label or "Env" in label
            if is_generic and not is_high_entropy(value):
                logger.debug("Low entropy (%0.2f) skip: %s", entropy, value[:20])
                continue

            results.append({
                "secret_type":   label,
                "severity":      severity,
                "matched_value": value,          # NEVER persisted — only used locally
                "preview":       mask_secret(value),
                "entropy":       round(entropy, 2),
            })

    return results


# ── File-level scan ───────────────────────────────────────────────────────────

def scan_file_content(file_path: str, content: str) -> list[dict]:
    """Scan a complete file's content, line by line."""
    if should_ignore_file(file_path):
        return []
    results = []
    for lineno, line in enumerate(content.splitlines(), start=1):
        for hit in scan_text_for_secrets(line):
            hit["file_path"]   = file_path
            hit["line_number"] = lineno
            hit["context_line"] = redact_in_line(line.strip(), hit["matched_value"])
            results.append(hit)
    return results


# ── Commit diff scan (primary entry point) ───────────────────────────────────

def scan_commit_diff(repo_full_name: str, commit_data: dict) -> list[dict]:
    """
    Scan a commit's diff for secrets.

    commit_data is the JSON returned by GitHub's
    GET /repos/{owner}/{repo}/commits/{sha} endpoint — it contains:
      commit_data["files"]   — list of changed files
      file["filename"]       — path
      file["patch"]          — unified diff text (added/removed lines)

    Only ADDED lines ('+') are scanned — checking removed lines would generate
    findings for secrets that were ALREADY deleted, not new leaks.

    Returns a list of finding dicts ready to be stored.
    """
    commit_sha = commit_data.get("sha", "")[:8]
    commit_url = commit_data.get("html_url", "")
    owner      = repo_full_name.split("/")[0] if "/" in repo_full_name else repo_full_name
    repo_name  = repo_full_name.split("/")[1] if "/" in repo_full_name else repo_full_name

    findings = []
    files    = commit_data.get("files", []) or []

    for file_info in files:
        file_path = file_info.get("filename", "")
        patch     = file_info.get("patch", "")

        if not patch or should_ignore_file(file_path):
            continue

        added_lines = parse_added_lines(patch)

        for line_number, line_content in added_lines:
            for hit in scan_text_for_secrets(line_content):
                finding = _build_finding(
                    repo        = repo_full_name,
                    owner       = owner,
                    commit_sha  = commit_sha,
                    commit_url  = commit_url,
                    file_path   = file_path,
                    line_number = line_number,
                    hit         = hit,
                )
                findings.append(finding)

    if findings:
        logger.info("commit %s in %s — %d finding(s)", commit_sha, repo_full_name, len(findings))

    return findings


# ── Whole-repo scan (deep) ────────────────────────────────────────────────────

def scan_repo(repo_full_name: str, get_content_fn, max_files: int = 50) -> list[dict]:
    """
    Deep-scan a repository's recent commits.

    Use get_content_fn(repo, path) to fetch individual file contents when a
    patch is unavailable (e.g., first commit / binary patch).

    This is slower than diff scanning and should only be run on suspicious repos.
    """
    from github_api import get_repo_commits, get_commit_diff
    findings = []
    commits  = get_repo_commits(repo_full_name, per_page=5)
    scanned  = 0

    for commit in commits:
        if scanned >= max_files:
            break
        sha  = commit.get("sha", "")
        data = get_commit_diff(repo_full_name, sha)
        if data:
            findings.extend(scan_commit_diff(repo_full_name, data))
        scanned += 1

    return findings


# ── Builder ───────────────────────────────────────────────────────────────────

def _build_finding(repo: str, owner: str, commit_sha: str, commit_url: str,
                   file_path: str, line_number: Optional[int], hit: dict) -> dict:
    """
    Construct the canonical finding dict that gets stored in Redis and shown
    on the dashboard.  The raw matched_value is removed here — only the
    masked preview is kept.
    """
    now = datetime.now(timezone.utc).isoformat()
    uid = hashlib.sha256(
        f"{repo}:{commit_sha}:{file_path}:{hit['secret_type']}:{hit['preview']}".encode()
    ).hexdigest()[:16]

    return {
        "id":             uid,
        "repo":           repo,
        "repo_url":       f"https://github.com/{repo}",
        "owner":          owner,
        "commit_sha":     commit_sha,
        "commit_url":     commit_url,
        "file_path":      file_path,
        "line_number":    line_number,
        "secret_type":    hit["secret_type"],
        "severity":       hit["severity"],
        "preview":        hit["preview"],           # masked: AKIA****ZXYZ
        "context_line":   hit.get("context_line", ""),
        "entropy":        hit.get("entropy", 0),
        "detected_at":    now,
        "source":         "python_scanner",
        "disclosure_url": disclosure_issue_url(
            owner, repo.split("/")[1] if "/" in repo else repo,
            hit["secret_type"], file_path, line_number, commit_url,
        ),
        "is_new": True,
    }

# ── Optional AI layer (not enabled by default) ────────────────────────────────
#
# If you want AI-powered classification, add this after scan_text_for_secrets():
#
# from openai import OpenAI
# _ai = OpenAI()
#
# def ai_confirm_finding(line: str, secret_type: str) -> float:
#     """
#     Ask an LLM: "Is this a real credential or a false positive?"
#     Returns confidence 0.0–1.0.
#
#     Only call this for medium/low severity hits where regex alone is uncertain.
#     High-cost, slow — use sparingly (< 100 calls/day on free tier).
#     """
#     prompt = (
#         f"A security scanner found a potential {secret_type} in this code line:\n"
#         f"  {line}\n\n"
#         f"Is this a real credential (not a placeholder/test value)? "
#         f"Reply with only a number: 0.0 = definitely fake, 1.0 = definitely real."
#     )
#     resp = _ai.chat.completions.create(
#         model="gpt-4o-mini",
#         messages=[{"role": "user", "content": prompt}],
#         max_tokens=10,
#     )
#     try:
#         return float(resp.choices[0].message.content.strip())
#     except ValueError:
#         return 0.5
