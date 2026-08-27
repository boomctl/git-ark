use anyhow::{anyhow, Result};
use std::io::Write;
use std::path::Path;

use crate::clock::Clock;
use crate::config::Config;
use crate::store::ObjectStore;
use crate::{crypto, git};

pub fn repo_name(repos_root: &Path, repo: &Path) -> Result<String> {
    let rel = repo.strip_prefix(repos_root).map_err(|_| {
        anyhow!(
            "repo {} not under root {}",
            repo.display(),
            repos_root.display()
        )
    })?;
    let s = rel.to_string_lossy();
    Ok(s.strip_suffix(".git").unwrap_or(&s).to_string())
}

pub fn latest_key(prefix: &str, name: &str) -> String {
    format!("{prefix}/{name}/latest.age")
}

pub fn history_key(prefix: &str, name: &str, ts: &str) -> String {
    format!("{prefix}/{name}/history/{ts}.age")
}

/// Parse post-receive stdin (`<old-sha> <new-sha> <refname>` per line) into the
/// list of updated ref names.
pub fn parse_receive_refs(input: &str) -> Vec<String> {
    input
        .lines()
        .filter_map(|l| l.split_whitespace().nth(2).map(str::to_string))
        .collect()
}

/// Does a ref name (e.g. `refs/heads/main`) match a `backup_refs` pattern?
/// `**`/`*` match everything; a full ref matches exactly; a bare branch name
/// (`main`) matches `refs/heads/<name>`.
pub fn ref_matches(refname: &str, pattern: &str) -> bool {
    pattern == "**"
        || pattern == "*"
        || pattern == refname
        || format!("refs/heads/{pattern}") == refname
}

/// Should a push that updated `updated_refs` trigger the S3 backup, given the
/// configured `backup_refs`?
pub fn should_back_up(updated_refs: &[String], backup_refs: &[String]) -> bool {
    updated_refs
        .iter()
        .any(|r| backup_refs.iter().any(|p| ref_matches(r, p)))
}

/// Which refs the S3 bundle should contain, derived from the effective
/// `backup_refs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleSelection {
    /// Bundle every ref (`git bundle create - --all`) — the `**`/`*` case.
    All,
    /// Bundle exactly these concrete, existing refs (may be empty → no-op).
    Refs(Vec<String>),
}

/// Resolve the effective `backup_refs` into a concrete bundle selection so the
/// S3 bundle contains ONLY the selected branches (a non-selected branch is then
/// genuinely absent from S3).
///
/// A `**`/`*` pattern means "everything" → [`BundleSelection::All`]. Otherwise
/// each pattern is resolved to a concrete, existing ref: a full `refs/…` is used
/// as-is when it exists; a bare name (`main`) becomes `refs/heads/<name>` when it
/// exists; patterns whose ref doesn't exist are dropped. Order is preserved and
/// duplicates removed. An empty result is a no-op (the caller handles it).
pub fn resolve_bundle_refs(repo: &Path, backup_refs: &[String]) -> Result<BundleSelection> {
    if backup_refs.iter().any(|p| p == "**" || p == "*") {
        return Ok(BundleSelection::All);
    }
    let mut refs: Vec<String> = Vec::new();
    for pattern in backup_refs {
        let candidate = if pattern.starts_with("refs/") {
            pattern.clone()
        } else {
            format!("refs/heads/{pattern}")
        };
        if git::ref_exists(repo, &candidate) && !refs.contains(&candidate) {
            refs.push(candidate);
        }
    }
    Ok(BundleSelection::Refs(refs))
}

/// Build the per-ref summary block printed to the hook's stdout after a push —
/// one line per pushed ref (in receive order) showing which durable tiers it
/// reached.
///
/// Every pushed ref lands on the host (NAS). A ref additionally shows "encrypted
/// S3" when it matches the effective `backup_refs`, and "GitHub" when its branch
/// is among the mirror's configured branches. `✓` marks a ref that went beyond
/// NAS; `○` marks a NAS-only ref. The ref column is left-aligned and padded to
/// the longest pushed ref name (min 14).
pub fn summarize_refs(
    updated: &[String],
    effective_backup_refs: &[String],
    github_branches: &[String],
) -> String {
    // Display name: drop `refs/heads/`; other refs (e.g. tags) are shown whole.
    let display = |r: &str| r.strip_prefix("refs/heads/").unwrap_or(r).to_string();
    let names: Vec<String> = updated.iter().map(|r| display(r)).collect();
    let width = names
        .iter()
        .map(|n| n.chars().count())
        .max()
        .unwrap_or(0)
        .max(14);

    let mut lines = Vec::with_capacity(updated.len());
    for (r, name) in updated.iter().zip(&names) {
        let s3 = effective_backup_refs.iter().any(|p| ref_matches(r, p));
        let gh = github_branches
            .iter()
            .any(|b| r.strip_prefix("refs/heads/") == Some(b.as_str()) || b == r);

        let mut tiers = vec!["NAS"];
        if s3 {
            tiers.push("encrypted S3");
        }
        if gh {
            tiers.push("GitHub");
        }
        let mark = if s3 || gh { "✓" } else { "○" };
        let tier_str = if tiers.len() == 1 {
            "NAS only".to_string()
        } else {
            tiers.join(" + ")
        };
        lines.push(format!("{mark} {name:<width$}  {tier_str}"));
    }
    lines.join("\n")
}

pub fn run_backup(
    cfg: &Config,
    repo: &Path,
    backup_refs: &[String],
    store: &dyn ObjectStore,
    clock: &dyn Clock,
    out: &mut dyn Write,
) -> Result<()> {
    let name = repo_name(&cfg.repos_root, repo)?;

    // Bundle only the selected refs (not the whole repo) so a non-selected
    // branch stays off S3 entirely.
    let selection = resolve_bundle_refs(repo, backup_refs)?;

    writeln!(out, "bundling {name} …").ok();
    out.flush().ok();
    let bundle = match &selection {
        BundleSelection::All => git::bundle_all(repo)?,
        BundleSelection::Refs(refs) => {
            if refs.is_empty() {
                // Nothing selected exists — a no-op. run_backup is only reached
                // when a pushed ref matched, so this is defensive.
                writeln!(out, "○ nothing to back up — no selected refs exist").ok();
                out.flush().ok();
                return Ok(());
            }
            git::bundle_refs(repo, refs)?
        }
    };

    writeln!(out, "encrypting {} bytes …", bundle.len()).ok();
    out.flush().ok();
    let ciphertext = crypto::encrypt(&cfg.age_recipient, &bundle)?;

    let ts = clock.timestamp();
    let hist = history_key(&cfg.s3.prefix, &name, &ts);
    let latest = latest_key(&cfg.s3.prefix, &name);
    // History first, then latest — latest is the "known-good newest" pointer.
    store.put(&hist, &ciphertext)?;
    store.put(&latest, &ciphertext)?;

    writeln!(
        out,
        "✓ backup → s3://{}/{}  +  {}",
        cfg.s3.bucket, latest, hist
    )
    .ok();
    out.flush().ok();
    Ok(())
}

#[cfg(test)]
mod key_tests {
    use super::*;
    use std::path::Path;
    #[test]
    fn repo_name_strips_root_and_git() {
        let n = repo_name(
            Path::new("/srv/repos"),
            Path::new("/srv/repos/team/app.git"),
        )
        .unwrap();
        assert_eq!(n, "team/app");
    }
    #[test]
    fn key_layout() {
        assert_eq!(latest_key("git-ark", "app"), "git-ark/app/latest.age");
        assert_eq!(
            history_key("git-ark", "app", "2026-01-02T03-04-05Z"),
            "git-ark/app/history/2026-01-02T03-04-05Z.age"
        );
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    fn defaults() -> Vec<String> {
        vec![
            "refs/heads/main".to_string(),
            "refs/heads/master".to_string(),
        ]
    }
    fn refs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_receive_ref_lines() {
        let input = "aaa bbb refs/heads/main\nccc ddd refs/heads/feature\n";
        assert_eq!(
            parse_receive_refs(input),
            refs(&["refs/heads/main", "refs/heads/feature"])
        );
    }

    #[test]
    fn ref_matching_rules() {
        assert!(ref_matches("refs/heads/main", "main"));
        assert!(ref_matches("refs/heads/main", "refs/heads/main"));
        assert!(ref_matches("refs/heads/anything", "**"));
        assert!(!ref_matches("refs/heads/feature", "main"));
        assert!(!ref_matches("refs/tags/v1", "main"));
    }

    #[test]
    fn gates_on_main_master_by_default() {
        assert!(should_back_up(&refs(&["refs/heads/main"]), &defaults()));
        assert!(should_back_up(&refs(&["refs/heads/master"]), &defaults()));
        assert!(!should_back_up(&refs(&["refs/heads/feature"]), &defaults()));
        // A push touching several refs including main is gated in.
        assert!(should_back_up(
            &refs(&["refs/heads/feature", "refs/heads/main"]),
            &defaults()
        ));
    }

    #[test]
    fn double_star_backs_up_every_ref() {
        let all = vec!["**".to_string()];
        assert!(should_back_up(&refs(&["refs/heads/feature"]), &all));
        assert!(should_back_up(&refs(&["refs/tags/v1"]), &all));
    }
}

#[cfg(test)]
mod bundle_selection_tests {
    use super::*;
    use std::process::Command;

    /// A repo with a `main` and a `feature` branch.
    fn repo_with_branches(dir: &Path) {
        let g = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(dir)
                    .status()
                    .unwrap()
                    .success(),
                "git {args:?}"
            );
        };
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "t"]);
        std::fs::write(dir.join("a.txt"), "a").unwrap();
        g(&["add", "."]);
        g(&["commit", "-qm", "a"]);
        g(&["branch", "feature"]);
    }

    fn strs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn double_star_or_star_selects_all() {
        let d = tempfile::tempdir().unwrap();
        repo_with_branches(d.path());
        assert_eq!(
            resolve_bundle_refs(d.path(), &strs(&["**"])).unwrap(),
            BundleSelection::All
        );
        assert_eq!(
            resolve_bundle_refs(d.path(), &strs(&["*"])).unwrap(),
            BundleSelection::All
        );
        // A `**` anywhere in the list wins.
        assert_eq!(
            resolve_bundle_refs(d.path(), &strs(&["main", "**"])).unwrap(),
            BundleSelection::All
        );
    }

    #[test]
    fn resolves_bare_and_full_refs_that_exist() {
        let d = tempfile::tempdir().unwrap();
        repo_with_branches(d.path());
        let sel = resolve_bundle_refs(d.path(), &strs(&["main", "refs/heads/feature"])).unwrap();
        assert_eq!(
            sel,
            BundleSelection::Refs(strs(&["refs/heads/main", "refs/heads/feature"]))
        );
    }

    #[test]
    fn drops_nonexistent_and_dedups() {
        let d = tempfile::tempdir().unwrap();
        repo_with_branches(d.path());
        // `main` exists (listed twice → deduped); `nope` and `refs/heads/ghost`
        // don't exist and are dropped.
        let sel = resolve_bundle_refs(
            d.path(),
            &strs(&["main", "nope", "refs/heads/ghost", "refs/heads/main"]),
        )
        .unwrap();
        assert_eq!(sel, BundleSelection::Refs(strs(&["refs/heads/main"])));
    }

    #[test]
    fn empty_when_nothing_selected_exists() {
        let d = tempfile::tempdir().unwrap();
        repo_with_branches(d.path());
        let sel = resolve_bundle_refs(d.path(), &strs(&["ghost", "refs/tags/none"])).unwrap();
        assert_eq!(sel, BundleSelection::Refs(Vec::new()));
    }
}

#[cfg(test)]
mod summary_tests {
    use super::*;

    fn strs(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn three_tier_example() {
        let updated = strs(&[
            "refs/heads/main",
            "refs/heads/big-feature",
            "refs/heads/scratch",
        ]);
        let backup_refs = strs(&["main", "big-feature"]);
        let github = strs(&["main"]);
        let out = summarize_refs(&updated, &backup_refs, &github);
        // Column padded to the longest name (`big-feature`, 11) but min 14, then
        // two spaces before the tier list.
        let expected = "\
✓ main            NAS + encrypted S3 + GitHub
✓ big-feature     NAS + encrypted S3
○ scratch         NAS only";
        assert_eq!(out, expected);
    }

    #[test]
    fn branch_only_push_is_all_nas_only() {
        // Nothing matches backup_refs or github → every ref is ○ NAS only.
        let updated = strs(&["refs/heads/scratch", "refs/heads/wip"]);
        let out = summarize_refs(&updated, &strs(&["main"]), &[]);
        let expected = "\
○ scratch         NAS only
○ wip             NAS only";
        assert_eq!(out, expected);
    }

    #[test]
    fn double_star_marks_every_ref_s3() {
        let updated = strs(&["refs/heads/main", "refs/tags/v1"]);
        let out = summarize_refs(&updated, &strs(&["**"]), &[]);
        // Tags keep their full name; both go to S3 under `**`.
        assert!(out.contains("✓ main"));
        assert!(out.lines().all(|l| l.starts_with('✓')));
        assert!(out.contains("refs/tags/v1"));
        assert!(out.lines().all(|l| l.contains("NAS + encrypted S3")));
    }
}
