//! Offline scorer for the local judge — the measurement half of
//! `benchmarks/local-judge/`.
//!
//! Reads text pairs as JSON lines and writes the model's raw logits, through
//! the same [`xerj_ai::judge::JudgeRuntime`] the server scores with (same
//! tokenizer settings, same length-aware batching, same thread pool), so a
//! number computed from this output is a number about the shipped code path.
//!
//! ```text
//! cargo run --release -p xerj-ai --features neural --example pair_score -- \
//!     --task rerank --tier small --in pairs.jsonl --out logits.jsonl
//! ```
//!
//! Input, one object per line: `{"g": "<group>", "id": "<pair id>", "a": "...", "b": "..."}`.
//! Consecutive lines with the same `g` are scored in ONE call — one group is
//! one rerank request — and the per-call wall time is what the latency
//! percentiles in the summary are taken over.
//!
//! Output, one object per line: `{"g", "id", "logits": [..]}`. The run is
//! resumable: groups already complete in `--out` are skipped, because the large
//! tier takes hours on a CPU and a run that cannot survive an interruption is
//! a run that gets quietly shortened instead.

use std::collections::HashSet;
use std::io::{BufRead, BufWriter, Write};
use std::time::{Duration, Instant};

use xerj_ai::judge::{model_spec, JudgeRuntime, JudgeRuntimeConfig, JudgeTask};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

fn peak_rss_mb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb / 1024)
}

fn percentile(sorted: &[Duration], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 * p) as usize).min(sorted.len() - 1);
    sorted[idx].as_secs_f64() * 1000.0
}

struct Row {
    g: String,
    id: String,
    a: String,
    b: String,
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let task = match arg(&args, "--task").as_deref() {
        Some("entail") => JudgeTask::Entail,
        _ => JudgeTask::Rerank,
    };
    let tier = arg(&args, "--tier").unwrap_or_else(|| "small".into());
    let input = arg(&args, "--in").ok_or_else(|| anyhow::anyhow!("--in <pairs.jsonl>"))?;
    let output = arg(&args, "--out").ok_or_else(|| anyhow::anyhow!("--out <logits.jsonl>"))?;
    let threads: usize = arg(&args, "--threads")
        .and_then(|t| t.parse().ok())
        .unwrap_or(0);
    let limit_groups: usize = arg(&args, "--max-groups")
        .and_then(|t| t.parse().ok())
        .unwrap_or(usize::MAX);
    let summary_path = arg(&args, "--summary");

    let spec = model_spec(task, &tier)
        .ok_or_else(|| anyhow::anyhow!("no {} model named `{tier}`", task.as_str()))?;

    // Resume: a group is complete when every one of its ids is in the output.
    let mut done: HashSet<(String, String)> = HashSet::new();
    if let Ok(f) = std::fs::File::open(&output) {
        for line in std::io::BufReader::new(f).lines() {
            let line = line?;
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                if let (Some(g), Some(id)) = (v["g"].as_str(), v["id"].as_str()) {
                    done.insert((g.to_string(), id.to_string()));
                }
            }
        }
    }

    let mut groups: Vec<Vec<Row>> = Vec::new();
    for line in std::io::BufReader::new(std::fs::File::open(&input)?).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(&line)?;
        let row = Row {
            g: v["g"].as_str().unwrap_or("").to_string(),
            id: v["id"].as_str().unwrap_or("").to_string(),
            a: v["a"].as_str().unwrap_or("").to_string(),
            b: v["b"].as_str().unwrap_or("").to_string(),
        };
        match groups.last_mut() {
            Some(last) if last[0].g == row.g => last.push(row),
            _ => groups.push(vec![row]),
        }
    }
    let total_groups = groups.len();
    groups.retain(|g| {
        g.iter()
            .any(|r| !done.contains(&(r.g.clone(), r.id.clone())))
    });
    groups.truncate(limit_groups);
    eprintln!(
        "pair_score: {} `{}` ({}) — {} groups to score, {} already done",
        task.as_str(),
        tier,
        spec.repo,
        groups.len(),
        total_groups - groups.len().min(total_groups),
    );

    let runtime = JudgeRuntime::new(JudgeRuntimeConfig {
        threads,
        max_inflight: 1,
        ..Default::default()
    });
    let load_started = Instant::now();
    let model = runtime
        .model_blocking(spec)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let load_ms = load_started.elapsed().as_millis();
    eprintln!(
        "pair_score: loaded in {load_ms}ms, weights {} MB, sha256 {}, threads {}",
        model.weights_bytes() / 1_000_000,
        model.weights_sha256(),
        runtime.threads(),
    );

    let tokio = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    let mut out = BufWriter::new(
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&output)?,
    );

    let mut latencies: Vec<Duration> = Vec::with_capacity(groups.len());
    let mut sizes: Vec<usize> = Vec::with_capacity(groups.len());
    let (mut pairs, mut real_tokens, mut padded, mut truncated, mut passes) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
    let run_started = Instant::now();
    for (n, group) in groups.iter().enumerate() {
        let batch: Vec<(String, String)> =
            group.iter().map(|r| (r.a.clone(), r.b.clone())).collect();
        let started = Instant::now();
        // No deadline: a benchmark must score everything it reports on.
        let far = Instant::now() + Duration::from_secs(24 * 3600);
        let scored = tokio
            .block_on(runtime.score(model.clone(), batch, far))
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        latencies.push(started.elapsed());
        sizes.push(group.len());
        pairs += group.len();
        real_tokens += scored.stats.real_tokens;
        padded += scored.stats.padded_token_slots;
        truncated += scored.stats.truncated;
        passes += scored.stats.forward_passes;
        for (row, logits) in group.iter().zip(scored.logits) {
            let logits = logits.ok_or_else(|| anyhow::anyhow!("pair {} was not scored", row.id))?;
            writeln!(
                out,
                "{}",
                serde_json::json!({"g": row.g, "id": row.id, "logits": logits})
            )?;
        }
        out.flush()?;
        if (n + 1) % 25 == 0 || n + 1 == groups.len() {
            let elapsed = run_started.elapsed().as_secs_f64();
            let rate = pairs as f64 / elapsed.max(1e-9);
            let left = groups[n + 1..].iter().map(Vec::len).sum::<usize>() as f64;
            eprintln!(
                "pair_score: {}/{} groups, {pairs} pairs, {rate:.1} pairs/s, ETA {:.0}s",
                n + 1,
                groups.len(),
                left / rate.max(1e-9),
            );
        }
    }

    let elapsed = run_started.elapsed().as_secs_f64();
    latencies.sort();
    sizes.sort();
    let median_group_size = sizes.get(sizes.len() / 2).copied().unwrap_or(0);
    let summary = serde_json::json!({
        "task": task.as_str(),
        "tier": tier,
        "repo": spec.repo,
        "revision": spec.revision,
        "weights_sha256": model.weights_sha256(),
        "weights_mb_on_disk": model.weights_bytes() / 1_000_000,
        "threads": runtime.threads(),
        "load_ms": load_ms as u64,
        "groups": latencies.len(),
        "pairs": pairs,
        "median_group_size": median_group_size,
        "pairs_per_second": pairs as f64 / elapsed.max(1e-9),
        "per_call_ms": {
            "p50": percentile(&latencies, 0.50),
            "p95": percentile(&latencies, 0.95),
            "max": latencies.last().map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0),
        },
        "forward_passes": passes,
        "real_tokens": real_tokens,
        "padded_token_slots": padded,
        "truncated_pairs": truncated,
        "peak_rss_mb": peak_rss_mb(),
        "elapsed_s": elapsed,
    });
    let text = serde_json::to_string_pretty(&summary)?;
    println!("{text}");
    if let Some(path) = summary_path {
        std::fs::write(path, text + "\n")?;
    }
    Ok(())
}
