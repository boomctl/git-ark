use anyhow::{bail, Context, Result};
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
pub fn bundle_refs(repo: &Path, refs: &[String]) -> Result<Vec<u8>> {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(repo)
        .args(["bundle", "create", "-"])
        .args(refs);
    run(&mut cmd, "git bundle create")
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
