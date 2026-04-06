"""
notifier.py — report and notify about findings.

Strategy:
  1. save_to_redis()     — primary store: shared with the Rust web dashboard
  2. print_report()      — human-readable console summary
  3. github_issue_url()  — responsible disclosure link (no auto-posting)
  4. log_report()        — append JSON lines to a local log file
"""
import json
import logging
import os
from typing import Optional

logger = logging.getLogger("github_scanner.notifier")

# ── Redis ──────────────────────────────────────────────────────────────────────
try:
    import redis as _redis_lib
    _redis_available = True
except ImportError:
    _redis_available = False
    logger.warning("redis-py not installed — findings won't be saved to Redis")


class Notifier:
    def __init__(self, redis_url: str = "redis://localhost:6379",
                 log_file: Optional[str] = None):
        self._redis: Optional[object] = None
        self._log_file = log_file

        if _redis_available:
            try:
                import redis
                self._redis = redis.from_url(redis_url, decode_responses=True,
                                             socket_connect_timeout=3)
                self._redis.ping()  # type: ignore[union-attr]
                logger.info("Connected to Redis at %s", redis_url)
            except Exception as exc:
                logger.warning("Redis unavailable (%s) — console-only mode", exc)
                self._redis = None

    # ── Redis ──────────────────────────────────────────────────────────────────

    def save_to_redis(self, finding: dict) -> bool:
        """
        Push one finding to the shared Redis list used by the Rust dashboard.

        Key layout (matches Rust code):
          repo-radar:secrets        — LPUSH list of JSON findings (capped at 500)
          repo-radar:seen:{id}      — dedup id, expires after 6h

        The Rust /api/secrets endpoint reads from this same list, so Python-
        and Rust-discovered secrets both appear on the same dashboard.
        """
        if not self._redis:
            return False

        import config
        rid = finding.get("id", "")

        # Dedup check
        seen_key = f"{config.REDIS_SEEN_PREFIX}{rid}"
        try:
            if self._redis.exists(seen_key):  # type: ignore[union-attr]
                logger.debug("Already seen: %s", rid)
                return False

            # Store finding
            payload = json.dumps(finding, ensure_ascii=False)
            self._redis.lpush(config.REDIS_SECRETS_KEY, payload)   # type: ignore[union-attr]
            self._redis.ltrim(config.REDIS_SECRETS_KEY, 0, config.MAX_FINDINGS - 1)  # type: ignore[union-attr]
            self._redis.setex(seen_key, config.REDIS_SEEN_TTL, 1)  # type: ignore[union-attr]
            return True

        except Exception as exc:
            logger.warning("Redis write failed: %s", exc)
            return False

    # ── Console ───────────────────────────────────────────────────────────────

    @staticmethod
    def print_report(findings: list[dict]) -> None:
        """Pretty-print findings to the terminal."""
        if not findings:
            return

        SEV_COLOR = {
            "critical": "\033[91m",  # bright red
            "high":     "\033[93m",  # yellow
            "medium":   "\033[94m",  # blue
            "low":      "\033[37m",  # white
        }
        RESET = "\033[0m"

        print(f"\n{'═'*70}")
        print(f"  🔑  {len(findings)} SECRET(S) DETECTED")
        print(f"{'═'*70}")

        for f in findings:
            sev   = f.get("severity", "medium")
            color = SEV_COLOR.get(sev, "")
            print(f"\n  {color}[{sev.upper():8s}]{RESET}  {f['secret_type']}")
            print(f"  {'Repo':12s}: {f['repo']}")
            print(f"  {'File':12s}: {f['file_path']}" +
                  (f" (L{f['line_number']})" if f.get("line_number") else ""))
            print(f"  {'Commit':12s}: {f['commit_sha']}")
            print(f"  {'Preview':12s}: {f['preview']}  (entropy={f.get('entropy', '?')})")
            print(f"  {'Context':12s}: {f.get('context_line', '')}")
            print(f"  {'Disclosure':12s}: {f.get('disclosure_url', '')[:80]}…")

        print()

    # ── File log ──────────────────────────────────────────────────────────────

    def log_report(self, findings: list[dict]) -> None:
        """Append findings as JSON-lines to a local file."""
        if not self._log_file or not findings:
            return
        try:
            with open(self._log_file, "a", encoding="utf-8") as fh:
                for f in findings:
                    # Strip matched_value in case it leaked in
                    safe = {k: v for k, v in f.items() if k != "matched_value"}
                    fh.write(json.dumps(safe, ensure_ascii=False) + "\n")
        except OSError as exc:
            logger.warning("Could not write log file: %s", exc)

    # ── Bulk ─────────────────────────────────────────────────────────────────

    def handle_findings(self, findings: list[dict]) -> int:
        """Print + save + log all findings. Returns count of newly stored."""
        self.print_report(findings)
        self.log_report(findings)
        stored = 0
        for f in findings:
            if self.save_to_redis(f):
                stored += 1
        if stored:
            logger.info("Stored %d new finding(s) in Redis", stored)
        return stored
