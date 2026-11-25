//! One sentence for "the data-dir drive is at the flood stage" (#3282).
//!
//! Two field reports hit a nearly full data-dir drive: `xerj autoindex
//! --dry-run` printed `budget_bytes=0` and exited 4 without naming the disk,
//! and a node in flood stage left the agent to work out on its own that
//! `--data-dir` had to move. The CLI, the budget JSON and the node's startup
//! log all print this same text, so the drive, the percentage and the fix are
//! named the same way wherever the user meets the block.

/// Bytes needed on a `total`-byte volume with `avail` free before usage drops
/// under `flood_pct`. `0` when it already is.
pub fn bytes_to_free(total: u64, avail: u64, flood_pct: u8) -> u64 {
    let headroom = u64::from(100u8.saturating_sub(flood_pct.min(100)));
    let reserve = ((total as u128) * u128::from(headroom) / 100) as u64;
    reserve.saturating_sub(avail)
}

/// `true` when `used_pct` is at or over an enabled, non-zero flood stage.
pub fn at_flood_stage(used_pct: u64, flood_pct: u8, threshold_enabled: bool) -> bool {
    threshold_enabled && flood_pct > 0 && used_pct >= u64::from(flood_pct)
}

/// The flood-stage sentence. `consequence` says what the block costs the
/// caller (`0 bytes of index budget`, `writes are refused`). `total`/`avail`
/// give the amount to free; when unknown the sentence says "free space".
pub fn flood_stage_hint(
    data_dir: &str,
    used_pct: u64,
    flood_pct: u8,
    total_avail: Option<(u64, u64)>,
    consequence: &str,
) -> String {
    let dir = if data_dir.trim().is_empty() {
        "(path not reported)"
    } else {
        data_dir
    };
    let free = match total_avail {
        Some((total, avail)) => {
            // One percent past the watermark, so the freed space also clears
            // the 1% release hysteresis and leaves a non-zero budget.
            let need = bytes_to_free(total, avail, flood_pct.saturating_sub(1));
            format!("Free at least {} there", human(need.max(1)))
        }
        None => "Free space there".to_string(),
    };
    format!(
        "data-dir drive holding {dir} is {used_pct}% full (flood stage {flood_pct}%): \
         {consequence}. {free}, or restart the node with --data-dir on another drive."
    )
}

fn human(bytes: u64) -> String {
    const GB: f64 = (1u64 << 30) as f64;
    const MB: f64 = (1u64 << 20) as f64;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else {
        format!("{:.1} MB", (b / MB).max(0.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    #[test]
    fn names_drive_percent_amount_and_the_data_dir_fix() {
        // 100 GiB volume, 4 GiB free: 96% used against a 95% flood stage.
        let s = flood_stage_hint(
            r"C:\Users\me\.xerj\data",
            96,
            95,
            Some((100 * GIB, 4 * GIB)),
            "0 bytes of index budget",
        );
        assert!(s.starts_with(r"data-dir drive holding C:\Users\me\.xerj\data is 96% full"));
        assert!(
            s.contains("(flood stage 95%): 0 bytes of index budget."),
            "{s}"
        );
        // Under 94% needs 6 GiB free: 2 GiB more.
        assert!(s.contains("Free at least 2.0 GB there"), "{s}");
        assert!(s.ends_with("or restart the node with --data-dir on another drive."));
    }

    #[test]
    fn unknown_sizes_and_path_still_give_the_fix() {
        let s = flood_stage_hint("", 97, 95, None, "writes are refused");
        assert!(s.contains("(path not reported) is 97% full"), "{s}");
        assert!(s.contains("Free space there, or restart the node with --data-dir"));
    }

    #[test]
    fn flood_stage_only_when_enabled_and_reached() {
        assert!(at_flood_stage(96, 95, true));
        assert!(at_flood_stage(95, 95, true));
        assert!(!at_flood_stage(94, 95, true));
        assert!(!at_flood_stage(99, 95, false));
        assert!(!at_flood_stage(99, 0, true));
        assert_eq!(bytes_to_free(100 * GIB, 10 * GIB, 95), 0);
        assert_eq!(bytes_to_free(100 * GIB, 4 * GIB, 95), GIB);
    }
}
