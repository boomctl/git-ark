use anyhow::{anyhow, bail, Context, Result};
use std::path::Path;
use std::process::Command;

fn run(cmd: &mut Command, what: &str) -> Result<Vec<u8>> {
    let out = cmd.output().with_context(|| format!("spawning {what}"))?;
    if !out.status.success() {
        bail!(
            "{what} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(out.stdout)
}

pub fn init_bare(repo: &Path) -> Result<()> {
    if let Some(parent) = repo.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    run(
        Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .arg(repo),
        "git init --bare",
    )?;
    Ok(())
}

pub fn bundle_all(repo: &Path) -> Result<Vec<u8>> {
    run(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["bundle", "create", "-", "--all"]),
        "git bundle create",
    )
}

/// Bundle exactly the named refs (`git bundle create - <ref1> <ref2> …`). A
/// bundle of specific refs is a valid bundle: `git clone` of it yields those
/// branches and nothing else, so a non-selected branch is genuinely not in it.
///
/// A bundle of specific refs carries no `HEAD` line, and `git clone` of a
/// HEAD-less bundle checks out nothing — newer git leaves an empty working
/// tree. So include `HEAD` when it points at one of the selected refs: the
/// clone then checks out that (primary) branch, and because HEAD already
/// resolves to a selected ref, no unselected branch's objects are pulled in.
/// (When HEAD is detached or points at an unselected branch, HEAD is omitted
/// and [`clone_bundle`]'s checkout fallback recovers content on restore.)
pub fn bundle_refs(repo: &Path, refs: &[String]) -> Result<Vec<u8>> {
    let mut spec: Vec<String> = Vec::with_capacity(refs.len() + 1);
    if head_points_at_selected(repo, refs) {
        spec.push("HEAD".to_string());
    }
    spec.extend(refs.iter().cloned());

    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo)
        .args(["bundle", "create", "-"])
        .args(&spec);
    run(&mut cmd, "git bundle create")
}

/// Is the repo's `HEAD` a symbolic ref to one of `refs`? False when HEAD is
/// detached, points elsewhere, or on any error. Used to decide whether HEAD can
/// join a selected-ref bundle without dragging in an unselected branch.
fn head_points_at_selected(repo: &Path, refs: &[String]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|head| {
            let head = head.trim();
            refs.iter().any(|r| r == head)
        })
        .unwrap_or(false)
}

/// Does `refname` resolve to an existing object in `repo`? Uses
/// `git rev-parse --verify --quiet`, which exits non-zero (quietly) when the
/// ref doesn't exist. Any spawn failure is treated as "doesn't exist".
pub fn ref_exists(repo: &Path, refname: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", "--quiet", refname])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn clone_bundle(bundle: &Path, dest: &Path) -> Result<()> {
    run(
        Command::new("git").arg("clone").arg(bundle).arg(dest),
        "git clone <bundle>",
    )?;
    // A selected-ref bundle may carry no HEAD line; then `git clone` fetches the
    // branches but checks nothing out (empty working tree on newer git). Ensure a
    // branch is checked out so a restore is never a silently-empty directory.
    ensure_checked_out(dest)?;
    Ok(())
}

/// If the fresh clone at `dest` has no checked-out commit (HEAD-less bundle),
/// check out a branch — preferring `main`/`master`, else the first fetched —
/// so restore always yields real content instead of an empty working tree.
fn ensure_checked_out(dest: &Path) -> Result<()> {
    let has_head = Command::new("git")
        .arg("-C")
        .arg(dest)
        .args(["rev-parse", "--verify", "-q", "HEAD"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if has_head {
        return Ok(());
    }

    // No checkout happened. The bundle's refs landed under refs/remotes/origin/*.
    let listed = run(
        Command::new("git").arg("-C").arg(dest).args([
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/remotes/origin",
        ]),
        "git for-each-ref",
    )?;
    let listed = String::from_utf8_lossy(&listed);
    let branches: Vec<&str> = listed
        .lines()
        .map(str::trim)
        .filter(|b| !b.is_empty() && *b != "origin/HEAD")
        .collect();
    let pick = branches
        .iter()
        .find(|b| **b == "origin/main" || **b == "origin/master")
        .or_else(|| branches.first())
        .ok_or_else(|| anyhow!("restored bundle has no branch to check out"))?;
    let local = pick.strip_prefix("origin/").unwrap_or(pick);
    run(
        Command::new("git")
            .arg("-C")
            .arg(dest)
            .args(["checkout", "-b", local, pick]),
        "git checkout",
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{bundle_all, bundle_refs, clone_bundle, init_bare, ref_exists};
    use std::process::Command;

    fn commit_a_file(work: &std::path::Path) {
        let sh = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(work)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {:?} failed", args);
        };
        sh(&["init", "-q"]);
        sh(&["config", "user.email", "t@t"]);
        sh(&["config", "user.name", "t"]);
        std::fs::write(work.join("hello.txt"), "hi").unwrap();
        sh(&["add", "."]);
        sh(&["commit", "-qm", "first"]);
    }

    #[test]
    fn bundle_then_clone_preserves_content() {
        let src = tempfile::tempdir().unwrap();
        commit_a_file(src.path());

        let bytes = bundle_all(src.path()).unwrap();
        assert!(bytes.starts_with(b"# v2 git bundle") || bytes.starts_with(b"# v3 git bundle"));

        let bdir = tempfile::tempdir().unwrap();
        let bpath = bdir.path().join("r.bundle");
        std::fs::write(&bpath, &bytes).unwrap();

        let out = tempfile::tempdir().unwrap();
        let dest = out.path().join("clone");
        clone_bundle(&bpath, &dest).unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.join("hello.txt")).unwrap(),
            "hi"
        );
    }

    #[test]
    fn init_bare_creates_repo() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("x.git");
        init_bare(&repo).unwrap();
        assert!(repo.join("HEAD").exists());
    }

    /// A repo with a `main` and a `feature` branch, each holding a distinct file.
    fn repo_with_two_branches(work: &std::path::Path) {
        let sh = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(work)
                    .status()
                    .unwrap()
                    .success(),
                "git {args:?}"
            );
        };
        sh(&["init", "-q", "-b", "main"]);
        sh(&["config", "user.email", "t@t"]);
        sh(&["config", "user.name", "t"]);
        std::fs::write(work.join("on_main.txt"), "m").unwrap();
        sh(&["add", "."]);
        sh(&["commit", "-qm", "main"]);
        sh(&["checkout", "-q", "-b", "feature"]);
        std::fs::write(work.join("on_feature.txt"), "f").unwrap();
        sh(&["add", "."]);
        sh(&["commit", "-qm", "feat"]);
        sh(&["checkout", "-q", "main"]);
    }

    #[test]
    fn bundle_refs_excludes_unselected_branch_and_still_restores() {
        // Bundle ONLY refs/heads/main; the feature branch must not be reachable
        // from the bundle, and a clone of it must check out main's content.
        let src = tempfile::tempdir().unwrap();
        repo_with_two_branches(src.path());

        let bytes = bundle_refs(src.path(), &["refs/heads/main".to_string()]).unwrap();
        assert!(bytes.starts_with(b"# v2 git bundle") || bytes.starts_with(b"# v3 git bundle"));

        let bdir = tempfile::tempdir().unwrap();
        let bpath = bdir.path().join("main.bundle");
        std::fs::write(&bpath, &bytes).unwrap();
        let dest = bdir.path().join("clone");
        clone_bundle(&bpath, &dest).unwrap();

        // main's file restored; the non-selected feature branch's file absent.
        assert_eq!(
            std::fs::read_to_string(dest.join("on_main.txt")).unwrap(),
            "m"
        );
        assert!(
            !dest.join("on_feature.txt").exists(),
            "feature branch leaked into the bundle"
        );
    }

    #[test]
    fn bundle_of_non_head_branch_still_restores_content() {
        // HEAD is on main, but we bundle ONLY feature — so the bundle has no
        // usable HEAD line. Restore must still check the branch out (via the
        // clone-side fallback), never leave an empty working tree.
        let src = tempfile::tempdir().unwrap();
        repo_with_two_branches(src.path()); // leaves HEAD on main

        let bytes = bundle_refs(src.path(), &["refs/heads/feature".to_string()]).unwrap();
        let bdir = tempfile::tempdir().unwrap();
        let bpath = bdir.path().join("feature.bundle");
        std::fs::write(&bpath, &bytes).unwrap();
        let dest = bdir.path().join("clone");
        clone_bundle(&bpath, &dest).unwrap();

        // feature's own file is present — i.e. something was actually checked out.
        assert_eq!(
            std::fs::read_to_string(dest.join("on_feature.txt")).unwrap(),
            "f"
        );
    }

    #[test]
    fn ref_exists_reports_presence() {
        let src = tempfile::tempdir().unwrap();
        repo_with_two_branches(src.path());
        assert!(ref_exists(src.path(), "refs/heads/main"));
        assert!(ref_exists(src.path(), "refs/heads/feature"));
        assert!(!ref_exists(src.path(), "refs/heads/ghost"));
        // A non-repo path never errors — just reports absence.
        let empty = tempfile::tempdir().unwrap();
        assert!(!ref_exists(empty.path(), "refs/heads/main"));
    }
}
