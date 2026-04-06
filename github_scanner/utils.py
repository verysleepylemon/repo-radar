"""
utils.py — shared helpers: entropy calculation, diff parsing, logging setup.

Shannon entropy lets us catch credentials that don't match a specific prefix
pattern but are statistically random (high bits-per-character = likely a key).
"""
import math
import logging
import re
import urllib.parse
from collections import Counter
from typing import Optional


def setup_logging(level: str = "INFO") -> logging.Logger:
    logging.basicConfig(
        format="%(asctime)s  %(levelname)-8s  %(name)s: %(message)s",
        datefmt="%H:%M:%S",
        level=getattr(logging, level.upper(), logging.INFO),
    )
    return logging.getLogger("github_scanner")


# ── Shannon entropy ────────────────────────────────────────────────────────────

def calculate_entropy(token: str) -> float:
    """
    Calculate Shannon entropy (bits per character).

    A truly random 32-char base64 string has ~5.5 bits/char.
    Normal English words are ~3.5-4.5.  A threshold of 3.5 catches most
    high-entropy credentials while avoiding common words.

    Formula:  H = -Σ p(x) * log2(p(x))
    """
    if not token:
        return 0.0
    counts = Counter(token)
    n = len(token)
    return -sum((c / n) * math.log2(c / n) for c in counts.values())


def is_high_entropy(token: str, threshold: float = 3.5, min_len: int = 20) -> bool:
    """Return True if the token looks statistically random."""
    return len(token) >= min_len and calculate_entropy(token) >= threshold


# ── Diff parsing ───────────────────────────────────────────────────────────────

def parse_added_lines(patch: str) -> list[tuple[Optional[int], str]]:
    """
    Parse a unified diff patch and return (line_number, content) for every
    line that was ADDED (starts with '+', not '+++').

    Only added lines matter — we're finding NEW secrets, not old ones.
    """
    results: list[tuple[Optional[int], str]] = []
    current_line: Optional[int] = None

    for raw in patch.splitlines():
        # Hunk header: @@ -old_start,old_count +new_start,new_count @@
        hunk = re.match(r"@@ -\d+(?:,\d+)? \+(\d+)(?:,\d+)? @@", raw)
        if hunk:
            current_line = int(hunk.group(1))
            continue

        if raw.startswith("+++"):
            continue

        if raw.startswith("+"):
            results.append((current_line, raw[1:]))  # strip leading '+'
            if current_line is not None:
                current_line += 1
        elif not raw.startswith("-"):
            # Context line — advance new-file counter
            if current_line is not None:
                current_line += 1

    return results


# ── Secret masking ─────────────────────────────────────────────────────────────

def mask_secret(value: str) -> str:
    """Return 'ABCD****WXYZ' — first 4 + stars + last 4."""
    if len(value) <= 8:
        return "*" * len(value)
    return value[:4] + "****" + value[-4:]


def redact_in_line(line: str, value: str) -> str:
    """Replace the actual secret in a line with [REDACTED]."""
    return line.replace(value, "[REDACTED]")


# ── GitHub URL helpers ─────────────────────────────────────────────────────────

def disclosure_issue_url(owner: str, repo: str, secret_type: str,
                          file_path: str, line_no: Optional[int],
                          commit_url: str) -> str:
    """Build a pre-filled GitHub 'new issue' URL for responsible disclosure."""
    title = f"[Security] Potential exposed credential: {secret_type}"
    body = (
        f"Hi,\n\n"
        f"I detected a potentially exposed credential in this repository.\n\n"
        f"| Field | Value |\n"
        f"|-------|-------|\n"
        f"| Type | `{secret_type}` |\n"
        f"| File | `{file_path}` |\n"
        f"| Line | `{line_no or 'unknown'}` |\n"
        f"| Commit | {commit_url} |\n\n"
        f"Please rotate this credential immediately if it is real.\n"
        f"The actual value was **not stored** — only a masked preview was recorded.\n\n"
        f"This is a responsible disclosure via "
        f"[repo-radar](https://github.com/lemwaiping123-eng/repo-radar)."
    )
    params = urllib.parse.urlencode({"title": title, "body": body})
    return f"https://github.com/{owner}/{repo}/issues/new?{params}"


# ── Ignore rules ──────────────────────────────────────────────────────────────

def should_ignore_file(path: str) -> bool:
    """Return True for generated/lock/dist files that are noisy."""
    from config import IGNORED_EXTENSIONS, IGNORED_PATH_FRAGMENTS
    lower = path.lower()
    if any(lower.endswith(ext) for ext in IGNORED_EXTENSIONS):
        return True
    return any(frag in lower for frag in IGNORED_PATH_FRAGMENTS)
