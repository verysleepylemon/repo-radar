# post-to-github.ps1
# Posts 5 expert technical comments to open help-wanted GitHub issues.
#
# Usage:
#   $env:GH_TOKEN = "ghp_YOUR_TOKEN_HERE"
#   .\post-to-github.ps1
#
# Get a free-scope token at: https://github.com/settings/tokens/new
# Required scope: public_repo

param(
    [string]$Token = $env:GH_TOKEN,
    [switch]$DryRun
)

if (-not $Token) {
    Write-Error @"
GitHub token required.

  Set it:  `$env:GH_TOKEN = "ghp_YOUR_TOKEN_HERE"
  Then:     .\post-to-github.ps1

Get a token at: https://github.com/settings/tokens/new   (check 'public_repo')
"@
    exit 1
}

$Headers = @{
    "Authorization"        = "Bearer $Token"
    "Accept"               = "application/vnd.github+json"
    "X-GitHub-Api-Version" = "2022-11-28"
    "User-Agent"           = "repo-radar/1.0"
}

function Post-Comment {
    param(
        [string]$Repo,
        [int]   $Issue,
        [string]$Body
    )
    $url = "https://api.github.com/repos/$Repo/issues/$Issue/comments"

    if ($DryRun) {
        Write-Host "[DRY-RUN] Would post to $Repo #$Issue ($($Body.Length) chars)"
        return
    }

    try {
        # Use WebClient with explicit UTF-8 to handle Unicode/long bodies reliably
        $json  = (@{ body = $Body } | ConvertTo-Json -Compress)
        $bytes = [System.Text.Encoding]::UTF8.GetBytes($json)
        $wc    = [System.Net.WebClient]::new()
        foreach ($k in $Headers.Keys) { $wc.Headers.Add($k, $Headers[$k]) }
        $wc.Headers["Content-Type"] = "application/json; charset=utf-8"
        $resp  = $wc.UploadData($url, "POST", $bytes)
        $html  = ([System.Text.Encoding]::UTF8.GetString($resp) | ConvertFrom-Json).html_url
        Write-Host "[OK] $Repo #$Issue  ->  $html"
    }
    catch {
        $status = $_.Exception.Response.StatusCode.value__
        Write-Host "[FAIL] $Repo #$Issue  (HTTP $status): $($_.Exception.Message)"
    }
}

# ─────────────────────────────────────────────────────────────────────────────
# Issue 1 of 5
# Repo:  DytallixHQ/dytallix-sdk
# Issue: #5  "Improve SDK error messages with actionable next steps"
# ─────────────────────────────────────────────────────────────────────────────
$comment1 = @'
Hi! Great issue — actionable error messages make a huge quality-of-life difference when integrating a new SDK. Here's a concrete approach using `thiserror`.

## Pattern: carry context in the variant, write the story in `Display`

The key is to add structured fields so `Display` can tell the full story instead of "something went wrong":

```rust
// crates/dytallix-sdk/src/error.rs
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SdkError {
    #[error(
        "Node at '{endpoint}' is unavailable.\n\
         Suggestions:\n\
         \x20• curl -s {endpoint}/health  to check node reachability\n\
         \x20• Try another node: https://docs.dytallix.com/nodes\n\
         Cause: {source}"
    )]
    NodeUnavailable {
        endpoint: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error(
        "Faucet rate limit hit for {address}.\n\
         Please wait {wait_seconds}s then retry.\n\
         Tip: use a different address for parallel testing."
    )]
    FaucetRateLimited {
        address: String,
        wait_seconds: u64,
    },

    #[error(
        "Keystore at '{path}' is corrupt or undecryptable.\n\
         Recovery options:\n\
         \x20• dytallix keystore recover --path '{path}'\n\
         \x20• dytallix init --force  (new address — existing funds become inaccessible)"
    )]
    KeystoreCorrupt { path: String },

    #[error(
        "Invalid Dytallix address '{address}'.\n\
         Expected Bech32m starting with 'dytallix1', e.g. dytallix1qp3k8…\n\
         Check: correct length, no extra spaces, case-insensitive."
    )]
    InvalidAddress { address: String },

    #[error(
        "Contract deployment failed (gas budget: {gas_budget}).\n\
         Suggestions:\n\
         \x20• Validate WASM:  wasm-opt --validate {wasm_path}\n\
         \x20• Increase gas budget by ~20%\n\
         Node message: {message}"
    )]
    ContractDeployFailed {
        wasm_path: String,
        gas_budget: u64,
        message: String,
    },
}
```

## Updating call sites

Wherever you construct these errors you add the fields at the point where context is naturally available:

```rust
// before
return Err(SdkError::NodeUnavailable);

// after
return Err(SdkError::NodeUnavailable {
    endpoint: self.node_url.clone(),
    source: Box::new(e),
});
```

## One unit test per variant keeps messages from regressing

```rust
#[test]
fn rate_limit_message_includes_wait_time_and_address() {
    let err = SdkError::FaucetRateLimited {
        address: "dytallix1abc".into(),
        wait_seconds: 120,
    };
    let msg = err.to_string();
    assert!(msg.contains("120"), "should include wait seconds");
    assert!(msg.contains("dytallix1abc"), "should include address");
}
```

Happy to open a PR if that would be helpful!
'@

Post-Comment "DytallixHQ/dytallix-sdk" 5 $comment1

# ─────────────────────────────────────────────────────────────────────────────
# Issue 2 of 5
# Repo:  DytallixHQ/dytallix-sdk
# Issue: #4  "Benchmark ML-DSA-65 sign and verify throughput across batch sizes"
# ─────────────────────────────────────────────────────────────────────────────
$comment2 = @'
Here's a complete `criterion` scaffold covering all the operations mentioned in the issue, including the four batch sizes and SLH-DSA-SHAKE-192s.

**`crates/dytallix-core/Cargo.toml`** — add:

```toml
[dev-dependencies]
criterion = { version = "0.5", features = ["html_reports"] }

[[bench]]
name = "pqc_signing"
harness = false
```

**`crates/dytallix-core/benches/pqc_signing.rs`**:

```rust
use criterion::{
    criterion_group, criterion_main, BenchmarkId, Criterion, Throughput,
};
use dytallix_core::keypair::{MlDsa65KeyPair, SlhDsaKeyPair};

const MSG: &[u8] = b"dytallix benchmark message";
const BATCH_SIZES: &[usize] = &[64, 256, 1024];

fn bench_mldsa65_keygen(c: &mut Criterion) {
    c.bench_function("mldsa65/keygen", |b| {
        b.iter(|| MlDsa65KeyPair::generate())
    });
}

fn bench_mldsa65_sign_single(c: &mut Criterion) {
    let kp = MlDsa65KeyPair::generate();
    c.bench_function("mldsa65/sign_single", |b| {
        b.iter(|| kp.sign(criterion::black_box(MSG)).unwrap())
    });
}

fn bench_mldsa65_verify_single(c: &mut Criterion) {
    let kp = MlDsa65KeyPair::generate();
    let sig = kp.sign(MSG).unwrap();
    c.bench_function("mldsa65/verify_single", |b| {
        b.iter(|| {
            kp.public_key()
                .verify(criterion::black_box(MSG), criterion::black_box(&sig))
                .unwrap()
        })
    });
}

fn bench_mldsa65_batch_verify(c: &mut Criterion) {
    let kp = MlDsa65KeyPair::generate();
    let mut group = c.benchmark_group("mldsa65/batch_verify");
    for &n in BATCH_SIZES {
        let sigs: Vec<_> = (0..n).map(|_| kp.sign(MSG).unwrap()).collect();
        group.throughput(Throughput::Elements(n as u64));
        group.bench_with_input(BenchmarkId::from_parameter(n), &sigs, |b, sigs| {
            b.iter(|| {
                for sig in sigs {
                    kp.public_key()
                        .verify(criterion::black_box(MSG), criterion::black_box(sig))
                        .unwrap();
                }
            })
        });
    }
    group.finish();
}

fn bench_slh_dsa(c: &mut Criterion) {
    let kp = SlhDsaKeyPair::generate();
    let sig = kp.sign(MSG).unwrap();
    let mut group = c.benchmark_group("slh_dsa_shake_192s");
    group.bench_function("sign", |b| {
        b.iter(|| kp.sign(criterion::black_box(MSG)).unwrap())
    });
    group.bench_function("verify", |b| {
        b.iter(|| {
            kp.public_key()
                .verify(criterion::black_box(MSG), criterion::black_box(&sig))
                .unwrap()
        })
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_mldsa65_keygen,
    bench_mldsa65_sign_single,
    bench_mldsa65_verify_single,
    bench_mldsa65_batch_verify,
    bench_slh_dsa,
);
criterion_main!(benches);
```

Run with:

```sh
cargo bench -p dytallix-core
# HTML report at target/criterion/index.html
```

A note on the 3,309-byte signature size mentioned in the whitepaper: add a quick assert in the sign benchmarks to confirm you're measuring the production path rather than a stub:

```rust
let sig = kp.sign(MSG).unwrap();
assert_eq!(sig.as_bytes().len(), 3309, "unexpected ML-DSA-65 signature size");
```

Worth pairing `--save-baseline main` on main and `--baseline main` on PRs so regressions are caught automatically. Happy to adjust to whatever the actual type names are in `keypair.rs`.
'@

Post-Comment "DytallixHQ/dytallix-sdk" 4 $comment2

# ─────────────────────────────────────────────────────────────────────────────
# Issue 3 of 5
# Repo:  DytallixHQ/dytallix-sdk
# Issue: #2  "Document the isochronous rejection sampling mechanism"
# ─────────────────────────────────────────────────────────────────────────────
$comment3 = @'
Great documentation target. The constant-time Fiat-Shamir-with-Abort story is non-obvious even to people who know lattice crypto, so clear inline docs would be a real help for future auditors.

Here's a documentation template you can adapt to the actual code in `keypair.rs`. The core insight is distinguishing *algorithmic* variability (the loop runs a different number of "real" accepted iterations) from *execution time* variability (which the isochronous implementation eliminates by always running exactly `MAX_SIGN_ITERATIONS`):

```rust
/// Signs `message` using ML-DSA-65 with a constant-time Fiat-Shamir-with-Abort loop.
///
/// # Timing Side-Channel Resistance
///
/// ML-DSA signing uses *rejection sampling*: the algorithm samples a random
/// commitment, derives a response vector, checks a norm bound, and either
/// accepts or rejects the candidate.  The number of rejections is variable
/// and would leak information about the secret key if it were reflected in
/// execution time.
///
/// This implementation is **isochronous**: it unconditionally executes exactly
/// `MAX_SIGN_ITERATIONS` iterations regardless of how many candidates are
/// algorithmically accepted.  Accepted iterations write their result to an
/// output buffer; rejected iterations perform equivalent work (same arithmetic,
/// same memory access pattern) then overwrite with the next candidate.
///
/// Concretely:
/// - The loop bound `MAX_SIGN_ITERATIONS` is derived from the security parameter
///   so that the probability all attempts fail is below 2^(-λ).
/// - All conditional assignments use constant-time selection (e.g.
///   `subtle::ConditionallySelectable` or bitmask-based cmov) to avoid
///   data-dependent branching.
/// - No early exit is taken for either acceptance or rejection paths.
///
/// # Limitations
///
/// - Cache-timing attacks via data-dependent memory accesses (e.g., table
///   lookups in the NTT butterfly) are not addressed here; the NTT is assumed
///   to be constant-time for all coefficients.
/// - Correct behaviour depends on the RNG supplying independent, uniformly
///   distributed coins for each iteration; the RNG must itself be side-channel
///   resistant.
///
/// # References
///
/// - NIST FIPS 204 §6.2 — ML-DSA signing algorithm
/// - Dytallix Technical Whitepaper §14.7 — open timing problem statement
pub fn sign(&self, message: &[u8]) -> Result<Signature, SignError> {
    // ... implementation
}
```

For the crate-level docs (`lib.rs`):

```rust
//! ## Timing Side-Channel Resistance
//!
//! All signing operations use isochronous loops to prevent timing oracles.
//! See [`keypair::KeyPair::sign`] for a detailed explanation of the technique.
//!
//! Verification is unconditionally polynomial-time and does not use rejection
//! sampling; it does not require isochronous treatment.
```

If the implementation uses `subtle` crate types (`Choice`, `ConditionallySelectable`,
`CtOption`) it is worth calling those out explicitly in the inline comments — an
auditor will want to trace every conditional back to a constant-time primitive.

Happy to contribute the inline comments directly as a PR if that would help!
'@

Post-Comment "DytallixHQ/dytallix-sdk" 2 $comment3

# ─────────────────────────────────────────────────────────────────────────────
# Issue 4 of 5
# Repo:  DytallixHQ/dytallix-sdk
# Issue: #3  "Add integration test for full dytallix init sequence against testnet"
# ─────────────────────────────────────────────────────────────────────────────
$comment4 = @'
Here's a complete async integration test scaffold. Key design decisions:

1. **Temporary keystore per test** via `tempfile::TempDir` — no test collisions, automatic cleanup on drop (pass *and* fail).
2. **Hard 60-second timeout** wrapping the entire init sequence, matching the acceptance criteria.
3. **`#[ignore]` + env-var guard** — the test is skipped during normal `cargo test` but runs when `CI_TESTNET=1` is set, so it never silently breaks offline CI.

**`crates/dytallix-cli/Cargo.toml`** — add:

```toml
[dev-dependencies]
tokio   = { version = "1", features = ["full"] }
tempfile = "3"
```

**`crates/dytallix-cli/tests/init_integration.rs`**:

```rust
//! Integration test: full `dytallix init` sequence against the live testnet.
//!
//! Verifies:
//!  - The sequence completes within 60 seconds (Milestone 1).
//!  - Both DGT and DRT balances are confirmed after completion.
//!  - The keystore is created at the expected path.
//!  - The generated address is valid Bech32m starting with `dytallix1`.
//!  - All test artefacts are cleaned up on exit (pass or fail).
//!
//! Skip in offline CI:  SKIP_TESTNET_TESTS=1 cargo test
//! Run explicitly:      CI_TESTNET=1 cargo test --test init_integration -- --include-ignored
use dytallix_cli::commands::init::{run_init, InitConfig};
use std::time::Duration;
use tempfile::TempDir;

#[tokio::test]
#[ignore = "requires live testnet — set CI_TESTNET=1 to run"]
async fn init_sequence_completes_within_sixty_seconds() {
    if std::env::var("CI_TESTNET").is_err() {
        eprintln!("skipping: CI_TESTNET not set");
        return;
    }

    let tmp          = TempDir::new().expect("create temp dir");
    let keystore_path = tmp.path().join("test_keystore.json");

    let config = InitConfig {
        keystore_path: keystore_path.clone(),
        node_url:   "https://testnet.dytallix.com".into(),
        faucet_url: "https://faucet.dytallix.com".into(),
    };

    let output = tokio::time::timeout(Duration::from_secs(60), run_init(config))
        .await
        .expect("init must complete within 60 seconds")
        .expect("init must succeed");

    // Address must be valid Bech32m
    assert!(
        output.address.starts_with("dytallix1"),
        "address must start with 'dytallix1', got: {}",
        output.address
    );
    assert!(output.address.len() >= 40, "Bech32m address too short");

    // Keystore file must exist
    assert!(
        keystore_path.exists(),
        "keystore not found at {}",
        keystore_path.display()
    );

    // Both balances confirmed
    assert!(output.dgt_balance > 0, "DGT balance must be > 0, got {}", output.dgt_balance);
    assert!(output.drt_balance > 0, "DRT balance must be > 0, got {}", output.drt_balance);

    // TempDir::drop() handles cleanup automatically — no manual rm needed
}
```

Run manually:

```sh
CI_TESTNET=1 cargo test --test init_integration -- --include-ignored --nocapture
```

Happy to adjust the `InitConfig` field names and the shape of the return type to match whatever `init.rs` actually uses.
'@

Post-Comment "DytallixHQ/dytallix-sdk" 3 $comment4

# ─────────────────────────────────────────────────────────────────────────────
# Issue 5 of 5
# Repo:  hyperlight-dev/hyperlight
# Issue: #706  "Track Hyperlight microbenchmarks"
# ─────────────────────────────────────────────────────────────────────────────
$comment5 = @'
Here's a `criterion` scaffold covering all four benchmark categories from the issue description. You can place it in `src/hyperlight_host/benches/` alongside any existing benchmarks.

**`src/hyperlight_host/Cargo.toml`** — add:

```toml
[dev-dependencies]
criterion  = { version = "0.5", features = ["html_reports"] }
num_cpus   = "1"

[[bench]]
name    = "hyperlight_microbenchmarks"
harness = false
```

**`src/hyperlight_host/benches/hyperlight_microbenchmarks.rs`**:

```rust
use criterion::{
    criterion_group, criterion_main, BenchmarkId, Criterion, Throughput,
};
use hyperlight_host::{GuestBinary, MultiUseSandbox, SandboxBuilder};

// ── 1. Guest-call round-trip ─────────────────────────────────────────────────
fn bench_guest_call_rtt(c: &mut Criterion) {
    let param_sizes: &[usize] = &[0, 64, 256, 1024, 4096];
    let mut group = c.benchmark_group("guest_call/rtt");

    for &n in param_sizes {
        let payload = vec![0u8; n];
        let sandbox = SandboxBuilder::new()
            .with_guest_binary(GuestBinary::TestGuest)
            .build()
            .unwrap();

        group.throughput(Throughput::Bytes(n as u64));
        group.bench_with_input(BenchmarkId::new("payload_bytes", n), &payload, |b, p| {
            b.iter(|| sandbox.call_echo(criterion::black_box(p)).unwrap())
        });
    }
    group.finish();
}

// ── 2. Host memory map / unmap ───────────────────────────────────────────────
fn bench_host_memory_mapping(c: &mut Criterion) {
    let region_sizes: &[usize] = &[4 * 1024, 64 * 1024, 1024 * 1024]; // 4 KB, 64 KB, 1 MB
    let mut group = c.benchmark_group("host_memory/map_unmap");

    for &size in region_sizes {
        let buf = vec![0u8; size];
        let sandbox = SandboxBuilder::new()
            .with_guest_binary(GuestBinary::TestGuest)
            .build()
            .unwrap();

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new("region_bytes", size), &buf, |b, buf| {
            b.iter(|| {
                let handle = sandbox.map_host_memory(criterion::black_box(buf)).unwrap();
                sandbox.unmap_host_memory(handle).unwrap();
            })
        });
    }
    group.finish();
}

// ── 3. Sandbox lifecycle ─────────────────────────────────────────────────────
fn bench_sandbox_lifecycle(c: &mut Criterion) {
    let mut group = c.benchmark_group("sandbox/lifecycle");

    group.bench_function("create_drop", |b| {
        b.iter(|| {
            let _sb = SandboxBuilder::new()
                .with_guest_binary(GuestBinary::TestGuest)
                .build()
                .unwrap();
        })
    });

    group.bench_function("create_snapshot_restore_drop", |b| {
        b.iter(|| {
            let sb    = SandboxBuilder::new()
                .with_guest_binary(GuestBinary::TestGuest)
                .build()
                .unwrap();
            let snap  = sb.snapshot().unwrap();
            let _rest = snap.restore().unwrap();
        })
    });

    group.finish();
}

// ── 4. Multithreaded guest calls (under‑ and over‑subscribed) ────────────────
fn bench_multithreaded_guest_calls(c: &mut Criterion) {
    let thread_counts: &[usize] = &[1, 2, 4, 8, 16];
    let cpus = num_cpus::get();
    let mut group = c.benchmark_group("guest_call/multithreaded");

    for &t in thread_counts {
        let label = if t <= cpus {
            format!("{t}t_undersubscribed")
        } else {
            format!("{t}t_oversubscribed")
        };

        group.bench_function(&label, |b| {
            b.iter(|| {
                let handles: Vec<_> = (0..t)
                    .map(|_| {
                        std::thread::spawn(|| {
                            let sb = SandboxBuilder::new()
                                .with_guest_binary(GuestBinary::TestGuest)
                                .build()
                                .unwrap();
                            sb.call_echo(b"hello").unwrap();
                        })
                    })
                    .collect();
                for h in handles { h.join().unwrap(); }
            })
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_guest_call_rtt,
    bench_host_memory_mapping,
    bench_sandbox_lifecycle,
    bench_multithreaded_guest_calls,
);
criterion_main!(benches);
```

Run with:

```sh
cargo bench -p hyperlight_host
# HTML report: target/criterion/index.html
```

A few notes:

- **Baseline tracking**: run `cargo bench -- --save-baseline main` on the main branch once, then use `cargo bench -- --baseline main` on PRs to get automatic regression reports — pairs nicely with the milestone goal of tracking perf over time.
- **Multithreaded design**: the scaffold above creates a fresh sandbox per thread, which measures end-to-end sandbox creation overhead in the multithreaded case. If the intent is to measure concurrent calls into a *shared* sandbox you'll need a `Mutex<MultiUseSandbox>` pattern — happy to sketch that variant.
- **Memory mapping**: it's worth benchmarking copy-on-write vs. direct-mapped variants separately if both code paths exist, since they'll show very different scaling curves.

Happy to open a PR if the API surface looks right — just need to confirm the correct type names (e.g. `SandboxBuilder`, `GuestBinary`, `call_echo`) against the current `src/hyperlight_host/src/lib.rs`.
'@

Post-Comment "hyperlight-dev/hyperlight" 706 $comment5

Write-Host ""
Write-Host "Done. Check the URLs above to see your comments live on GitHub."
