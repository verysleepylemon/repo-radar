//! Auto-replication module.
//!
//! When a confirmed or critical leak is newly detected via `/api/leaks`, this
//! module:
//!   1. Creates a local `replications/<leak_id>/` directory.
//!   2. Clones the leaked repo at depth=1 (fast, non-destructive).
//!   3. Reads source files and builds a compact context summary.
//!   4. Calls **OpenRouter's free model** to generate an idiomatic Rust port.
//!   5. Saves `leak_meta.json`, the cloned source, and `rust_port_scaffold.rs`.
//!
//! If `OPENROUTER_API_KEY` is not set, steps 3-5 are skipped and a placeholder
//! comment file is written instead — so the module is safe to use with zero
//! configuration.
//!
//! Errors are non-fatal: they are logged as warnings and the caller continues.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use anyhow::Result;
use serde_json::json;
use tracing::{info, warn};

use crate::web::LeakItem;

const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

/// Free-tier model on OpenRouter with strong code-gen ability.
/// Switch to another free model by setting REPLICATOR_MODEL env var.
const DEFAULT_FREE_MODEL: &str = "google/gemini-2.5-pro-exp-03-25:free";

/// Top-level output directory (relative to cwd).
const OUT_DIR: &str = "replications";

/// Maximum bytes of source context sent to the LLM.
const MAX_CONTEXT_BYTES: usize = 12_000;

// ─── Public API ───────────────────────────────────────────────────────────────

/// Shared set of leak IDs that have already been dispatched for replication.
/// Prevents re-triggering on every `/api/leaks` poll cycle.
pub type SeenReplications = Arc<RwLock<HashSet<String>>>;

pub fn new_seen_replications() -> SeenReplications {
    Arc::new(RwLock::new(HashSet::new()))
}

/// Handles one replication job.  Cheap to clone — wraps `Arc` internally.
#[derive(Clone)]
pub struct Replicator {
    http:    reqwest::Client,
    api_key: Option<String>,
    model:   String,
}

impl Replicator {
    pub fn new(http: reqwest::Client) -> Self {
        let api_key = std::env::var("OPENROUTER_API_KEY").ok();
        let model   = std::env::var("REPLICATOR_MODEL")
                        .unwrap_or_else(|_| DEFAULT_FREE_MODEL.to_string());
        if api_key.is_some() {
            info!(model = %model, "Replicator ready — OpenRouter LLM port generation enabled");
        } else {
            info!("Replicator ready — set OPENROUTER_API_KEY to enable LLM port generation");
        }
        Self { http, api_key, model }
    }

    /// Fire-and-forget entry point.  Spawned as a background task.
    pub async fn replicate(&self, leak: LeakItem) {
        if let Err(e) = self.try_replicate(&leak).await {
            warn!(leak_id = %leak.id, error = %e, "Replication failed (non-fatal)");
        }
    }

    // ─── Private helpers ──────────────────────────────────────────────────────

    async fn try_replicate(&self, leak: &LeakItem) -> Result<()> {
        // Sanitise leak.id for use as a directory name.
        let safe_id = leak.id.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "-");
        let out_path = PathBuf::from(OUT_DIR).join(&safe_id);
        tokio::fs::create_dir_all(&out_path).await?;

        // ── Step 1: persist metadata ──────────────────────────────────────────
        let meta = serde_json::to_string_pretty(leak)?;
        tokio::fs::write(out_path.join("leak_meta.json"), &meta).await?;
        info!(leak_id = %leak.id, path = %out_path.display(), "Replication started");

        // ── Step 2: clone the source repo ─────────────────────────────────────
        let clone_dir = out_path.join("source");
        let cloned   = self.git_clone(&leak.repo_url, &clone_dir).await;

        // ── Step 3: build source context for LLM ─────────────────────────────
        let source_ctx = if cloned {
            self.summarise_source(&clone_dir).await.unwrap_or_default()
        } else {
            // Fall back to description + root_cause when clone fails.
            format!("// Description:\n// {}\n\n// Root cause:\n// {}",
                leak.description, leak.root_cause)
        };

        // ── Step 4: LLM port generation ───────────────────────────────────────
        let scaffold = match &self.api_key {
            Some(key) => {
                self.generate_port(leak, &source_ctx, key).await
                    .unwrap_or_else(|e| {
                        warn!(error = %e, "LLM call failed; writing placeholder");
                        placeholder_comment(leak)
                    })
            }
            None => placeholder_comment(leak),
        };

        // ── Step 5: write output ───────────────────────────────────────────────
        let target_lang = leak.language.as_deref().unwrap_or("TypeScript");
        let scaffold_file = out_path.join(format!(
            "{}_to_rust_port.rs",
            target_lang.to_lowercase()
        ));
        tokio::fs::write(&scaffold_file, &scaffold).await?;

        println!("🔄 Replication saved → {}", out_path.display());
        if self.api_key.is_some() {
            println!("   └─ LLM port scaffold: {}", scaffold_file.display());
        } else {
            println!("   └─ Set OPENROUTER_API_KEY for automatic Rust port generation");
        }
        Ok(())
    }

    /// Clone `repo_url` into `target`, depth=1.  Returns true on success.
    async fn git_clone(&self, repo_url: &str, target: &PathBuf) -> bool {
        info!(url = %repo_url, target = %target.display(), "git clone --depth 1");
        matches!(
            tokio::process::Command::new("git")
                .args(["clone", "--depth", "1", "--quiet", repo_url,
                       target.to_str().unwrap_or(".")])
                .status()
                .await,
            Ok(s) if s.success()
        )
    }

    /// Walk the cloned directory and build a compact source summary (≤ MAX_CONTEXT_BYTES).
    async fn summarise_source(&self, dir: &PathBuf) -> Result<String> {
        const EXTS: &[&str] = &["ts", "js", "py", "go", "rs", "java", "cs", "cpp", "rb",
                                  "swift", "kt", "zig"];
        let mut summary = String::new();
        let mut stack   = vec![dir.clone()];

        'outer: while let Some(current) = stack.pop() {
            let mut entries = match tokio::fs::read_dir(&current).await {
                Ok(e) => e,
                Err(_) => continue,
            };
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.is_dir() {
                    // Skip hidden / dependency directories.
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if !name.starts_with('.') && name != "node_modules"
                        && name != "vendor" && name != "target" {
                        stack.push(path);
                    }
                    continue;
                }
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                if !EXTS.contains(&ext) { continue; }

                if let Ok(content) = tokio::fs::read_to_string(&path).await {
                    let preview: String = content.lines().take(150).collect::<Vec<_>>().join("\n");
                    let header = format!("\n// ═══ {} ═══\n", path.display());
                    summary.push_str(&header);
                    summary.push_str(&preview);
                    if summary.len() >= MAX_CONTEXT_BYTES { break 'outer; }
                }
            }
        }
        Ok(summary)
    }

    /// Call OpenRouter to generate a Rust port of the leaked code.
    async fn generate_port(
        &self,
        leak: &LeakItem,
        source_ctx: &str,
        api_key: &str,
    ) -> Result<String> {
        let src_lang    = leak.language.as_deref().unwrap_or("TypeScript");
        let prompt = format!(
            "You are an expert systems programmer specialising in Rust.\n\
             A leaked {src_lang} project has been publicly discovered: \"{name}\".\n\
             Description: {desc}\n\
             Root cause: {cause}\n\
             \n\
             Here is the source (truncated to the most important parts):\n\
             ```{src_lang}\n{ctx}\n```\n\
             \n\
             Task: Generate a complete, idiomatic, production-quality **Rust** port of this \
             project. Requirements:\n\
             - Use tokio for async runtime.\n\
             - Use anyhow for error handling.\n\
             - Use reqwest for HTTP.\n\
             - Use serde / serde_json for serialisation.\n\
             - Use clap for CLI argument parsing.\n\
             - Include a `Cargo.toml` dependency block as the opening comment.\n\
             - Add inline comments explaining non-obvious logic.\n\
             - Output ONLY valid Rust code — no markdown fences, no extra prose.",
            name  = leak.name,
            desc  = leak.description,
            cause = leak.root_cause,
            ctx   = &source_ctx[..source_ctx.len().min(MAX_CONTEXT_BYTES)],
        );

        let payload = json!({
            "model": self.model,
            "messages": [{ "role": "user", "content": prompt }],
            "max_tokens": 4096,
        });

        let resp = self.http
            .post(OPENROUTER_URL)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("HTTP-Referer", "https://github.com/lemwaiping123-eng/repo-radar")
            .header("X-Title", "repo-radar auto-replicator")
            .json(&payload)
            .send()
            .await?;

        if !resp.status().is_success() {
            anyhow::bail!("OpenRouter returned {}", resp.status());
        }

        let data: serde_json::Value = resp.json().await?;
        Ok(data["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("// LLM returned no content")
            .to_string())
    }
}

fn placeholder_comment(leak: &LeakItem) -> String {
    format!(
        "// ─── repo-radar auto-replicator ─────────────────────────────────────────────\n\
         // Set OPENROUTER_API_KEY to enable automatic LLM-powered Rust port generation.\n\
         //\n\
         // Leak : {name}\n\
         // Source language : {lang}\n\
         // Original repo  : {url}\n\
         // Description    : {desc}\n\
         // ─────────────────────────────────────────────────────────────────────────────\n\
         \n\
         // TODO: implement Rust port of {name}\n\
         fn main() {{}}\n",
        name = leak.name,
        lang = leak.language.as_deref().unwrap_or("Unknown"),
        url  = leak.repo_url,
        desc = leak.description.lines().next().unwrap_or(""),
    )
}
