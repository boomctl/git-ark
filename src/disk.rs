//! Filesystem free-space, via `df` — informational only (never blocks a backup).

use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::process::Command;

/// Free-space snapshot for the filesystem holding a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskUsage {
    pub total_bytes: u64,
    pub free_bytes: u64,
}

impl DiskUsage {
    /// Percent of the filesystem that is free, rounded down (0–100).
    pub fn percent_free(&self) -> u8 {
        if self.total_bytes == 0 {
            return 0;
        }
        ((self.free_bytes as u128 * 100) / self.total_bytes as u128) as u8
    }
}

/// Free space for the filesystem containing `path`, via `df -Pk` (POSIX,
/// 1024-byte blocks). No new dependency — same shell-out pattern as `git`.
pub fn usage(path: &Path) -> Result<DiskUsage> {
    let out = Command::new("df")
        .arg("-Pk")
        .arg(path)
        .output()
        .with_context(|| format!("running df on {}", path.display()))?;
    if !out.status.success() {
        return Err(anyhow!("df failed on {}", path.display()));
    }
    parse_df_pk(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| anyhow!("could not parse df output for {}", path.display()))
}

/// Parse `df -Pk` output. `-P` guarantees one physical line per filesystem:
/// `Filesystem 1024-blocks Used Available Capacity Mounted-on`. We anchor on the
/// Capacity column — the single `NN%` token — because the Mounted-on column that
/// follows it can contain spaces (e.g. `/Volumes/My Disk`), which a right-edge
/// anchor would miscount. Everything after `%` is ignored; the numeric columns
/// sit at fixed offsets before it. Non-numeric parses fall through to `None`.
fn parse_df_pk(text: &str) -> Option<DiskUsage> {
    let line = text.lines().nth(1)?;
    let cols: Vec<&str> = line.split_whitespace().collect();
    // Capacity is `NN%`; Available is the token before it, 1024-blocks three before.
    let cap = cols.iter().position(|c| c.ends_with('%'))?;
    if cap < 3 {
        return None;
    }
    let blocks: u64 = cols[cap - 3].parse().ok()?;
    let avail: u64 = cols[cap - 1].parse().ok()?;
    Some(DiskUsage {
        total_bytes: blocks.checked_mul(1024)?,
        free_bytes: avail.checked_mul(1024)?,
    })
}

/// Is the filesystem low? Low = proportionally AND absolutely low, so a huge
/// disk stays quiet until it is genuinely almost empty while a small one still
/// trips. See the plan's Global Constraints.
pub fn is_low(u: DiskUsage, warn_percent: u8, warn_min_free_bytes: u64) -> bool {
    (u.percent_free() as u32) < warn_percent as u32 && u.free_bytes < warn_min_free_bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real `df -Pk` output: header line, then one line per filesystem. Parsing
    // anchors on the Capacity (`NN%`) column.
    const DF: &str = "Filesystem 1024-blocks      Used Available Capacity Mounted on\n\
                      /dev/disk1s1 976490568 810000000 160000000      84% /\n";

    #[test]
    fn parses_df_pk_anchored_on_capacity() {
        let u = parse_df_pk(DF).unwrap();
        assert_eq!(u.total_bytes, 976_490_568 * 1024);
        assert_eq!(u.free_bytes, 160_000_000 * 1024);
    }

    #[test]
    fn parses_df_pk_with_spaces_in_mount_path() {
        // A "Mounted on" column with spaces must not shift the numeric columns —
        // the reproduced should-fix. Anchoring on the `%` token survives it.
        let df = "Filesystem 1024-blocks      Used Available Capacity Mounted on\n\
                  /dev/disk3s1 976490568 810000000 160000000      84% /Volumes/My Big Disk\n";
        let u = parse_df_pk(df).unwrap();
        assert_eq!(u.total_bytes, 976_490_568 * 1024);
        assert_eq!(u.free_bytes, 160_000_000 * 1024);
    }

    #[test]
    fn percent_free_is_rounded_down() {
        let u = DiskUsage {
            total_bytes: 1000,
            free_bytes: 165,
        };
        assert_eq!(u.percent_free(), 16);
    }

    #[test]
    fn low_requires_both_proportional_and_absolute() {
        // Huge disk, proportionally low but absolutely fine → NOT low.
        let huge = DiskUsage {
            total_bytes: 56_000_000_000_000,
            free_bytes: 5_600_000_000_000,
        };
        assert!(!is_low(huge, 15, 10 * 1024 * 1024 * 1024));
        // Small disk, both low → low.
        let small = DiskUsage {
            total_bytes: 20_000_000_000,
            free_bytes: 1_000_000_000,
        };
        assert!(is_low(small, 15, 10 * 1024 * 1024 * 1024));
        // Absolutely low but proportionally fine (tiny disk, half free) → NOT low.
        let tiny = DiskUsage {
            total_bytes: 4_000_000_000,
            free_bytes: 2_000_000_000,
        };
        assert!(!is_low(tiny, 15, 10 * 1024 * 1024 * 1024));
    }

    #[test]
    fn garbage_df_output_is_none() {
        assert!(parse_df_pk("nonsense\n").is_none());
        assert!(parse_df_pk("").is_none());
    }
}
