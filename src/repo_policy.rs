//! Per-repo backup + mirror policy, committed to the repo as `.git-ark.yml`.
//!
//! A repo declares its own policy in a `.git-ark.yml` at its root. On push the
//! hook reads that file straight out of the bare repo (`git show HEAD:…`) and
//! uses it to (a) override which refs earn the durable S3 backup and (b) mirror
//! named branches to GitHub. Absent (or unreadable) → no policy, and the caller
//! falls back to the central `backup_refs` with no mirror.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;
use std::process::Command;

/// A repo's committed `.git-ark.yml`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RepoPolicy {
    /// Refs whose push earns the durable, encrypted S3 backup. When non-empty
    /// this overrides the host's central `backup_refs`; empty → fall back to it.
    #[serde(default)]
    pub backup_refs: Vec<String>,
    /// Optional GitHub mirror. Omitted (or `enabled: false`) → no mirror.
    #[serde(default)]
    pub github: Option<GithubPolicy>,
}

/// The `github:` block of a `.git-ark.yml`.
#[derive(Debug, Clone, Deserialize)]
pub struct GithubPolicy {
    /// Set `false` to keep the block but turn mirroring off. Defaults to `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Visibility of the GitHub repo when git-ark creates it. Default: private.
    #[serde(default)]
    pub visibility: Visibility,
    /// GitHub org or user that owns the mirror.
    pub owner: String,
    /// GitHub repo name; defaults to this repo's own name when omitted.
    #[serde(default)]
    pub repo: Option<String>,
    /// Branches to push to the mirror.
    #[serde(default)]
    pub branches: Vec<String>,
}

fn default_true() -> bool {
    true
}

/// Visibility of a created GitHub mirror. Defaults to `Private` — a mirror
/// should never be more exposed than the operator asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    #[default]
    Private,
    Public,
}

impl<'de> Deserialize<'de> for Visibility {
    /// Accept `private`/`public` case-insensitively (`Private`, `PUBLIC`, …).
    fn deserialize<D>(d: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(d)?;
        match s.trim().to_ascii_lowercase().as_str() {
            "private" => Ok(Visibility::Private),
            "public" => Ok(Visibility::Public),
            other => Err(serde::de::Error::custom(format!(
                "invalid visibility {other:?}; expected 'private' or 'public'"
            ))),
        }
    }
}

impl RepoPolicy {
    /// Parse a `.git-ark.yml` document. A null document — an empty file,
    /// whitespace, or comments only — is a valid "no policy" and yields all
    /// defaults rather than an error. Deserializing into `Option` maps that null
    /// to `None` instead of failing on the absent struct.
    pub fn parse(yaml: &str) -> Result<RepoPolicy> {
        // Whitespace-only (incl. tabs, which YAML's scanner rejects as illegal
        // indentation) short-circuits to defaults before serde_yaml sees it.
        if yaml.trim().is_empty() {
            return Ok(RepoPolicy::default());
        }
        // A comments-only document is a valid null → map that to defaults too.
        let parsed: Option<RepoPolicy> =
            serde_yaml::from_str(yaml).context("parsing .git-ark.yml")?;
        Ok(parsed.unwrap_or_default())
    }
}

/// Read the committed `.git-ark.yml` from a bare repo's current `HEAD`.
///
/// Runs `git -C <repo> show HEAD:.git-ark.yml`. If the command fails — no such
/// file, or the repo has no `HEAD` yet — this is not an error: return `Ok(None)`
/// and let the caller fall back to the central config (no override, no mirror).
/// A file that exists but fails to parse *is* surfaced as an error.
pub fn read_repo_policy(repo: &Path) -> Result<Option<RepoPolicy>> {
    // The policy at the default branch's tip wins. A freshly `git init --bare`
    // repo can have HEAD pointing at an unborn `master` while the pushed branch
    // is `main`, so try main, then master, then HEAD.
    for refname in ["refs/heads/main", "refs/heads/master", "HEAD"] {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["show", &format!("{refname}:.git-ark.yml")])
            .output()
            .with_context(|| format!("spawning git show {refname}:.git-ark.yml"))?;
        if out.status.success() {
            let yaml = String::from_utf8_lossy(&out.stdout);
            return Ok(Some(RepoPolicy::parse(&yaml)?));
        }
    }
    // No committed policy on any candidate ref (missing file / no such ref).
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_policy_from_main_when_head_is_unborn_master() {
        use std::process::Command;
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        let bare = dir.path().join("r.git");
        let g = |args: &[&str], cwd: &std::path::Path| {
            assert!(
                Command::new("git").args(args).current_dir(cwd).status().unwrap().success(),
                "git {args:?}"
            );
        };
        std::fs::create_dir(&work).unwrap();
        g(&["init", "-q", "-b", "main"], &work);
        g(&["config", "user.email", "t@t"], &work);
        g(&["config", "user.name", "t"], &work);
        std::fs::write(work.join(".git-ark.yml"), "backup_refs:\n  - main\n").unwrap();
        g(&["add", "."], &work);
        g(&["commit", "-qm", "policy"], &work);
        assert!(Command::new("git")
            .args(["clone", "--bare", work.to_str().unwrap(), bare.to_str().unwrap()])
            .status().unwrap().success());
        // Reproduce the bug: HEAD points at an unborn `master`, content is on `main`.
        g(&["symbolic-ref", "HEAD", "refs/heads/master"], &bare);
        let policy = read_repo_policy(&bare).unwrap().expect("policy found via main");
        assert_eq!(policy.backup_refs, vec!["main".to_string()]);
    }

    #[test]
    fn parses_full_policy() {
        let yaml = r#"
backup_refs:
  - main
  - long-lived-feature
github:
  enabled: true
  visibility: private
  owner: some-org
  repo: custom-name
  branches:
    - main
"#;
        let p = RepoPolicy::parse(yaml).unwrap();
        assert_eq!(p.backup_refs, vec!["main", "long-lived-feature"]);
        let g = p.github.unwrap();
        assert!(g.enabled);
        assert_eq!(g.visibility, Visibility::Private);
        assert_eq!(g.owner, "some-org");
        assert_eq!(g.repo.as_deref(), Some("custom-name"));
        assert_eq!(g.branches, vec!["main"]);
    }

    #[test]
    fn minimal_github_applies_defaults() {
        // No `enabled`, no `visibility`, no `repo`, no `branches`.
        let yaml = r#"
github:
  owner: someone
"#;
        let p = RepoPolicy::parse(yaml).unwrap();
        assert!(p.backup_refs.is_empty());
        let g = p.github.unwrap();
        assert!(g.enabled, "enabled defaults to true");
        assert_eq!(g.visibility, Visibility::Private, "visibility defaults to private");
        assert_eq!(g.owner, "someone");
        assert_eq!(g.repo, None);
        assert!(g.branches.is_empty());
    }

    #[test]
    fn visibility_is_case_insensitive() {
        let cases = [
            ("private", Visibility::Private),
            ("Private", Visibility::Private),
            ("PRIVATE", Visibility::Private),
            ("public", Visibility::Public),
            ("Public", Visibility::Public),
            ("PUBLIC", Visibility::Public),
        ];
        for (word, want) in cases {
            let yaml = format!("github:\n  owner: o\n  visibility: {word}\n");
            let p = RepoPolicy::parse(&yaml).unwrap();
            assert_eq!(p.github.unwrap().visibility, want, "visibility {word:?}");
        }
    }

    #[test]
    fn rejects_unknown_visibility() {
        let yaml = "github:\n  owner: o\n  visibility: secret\n";
        assert!(RepoPolicy::parse(yaml).is_err());
    }

    #[test]
    fn no_github_block_yields_none() {
        let yaml = "backup_refs:\n  - main\n";
        let p = RepoPolicy::parse(yaml).unwrap();
        assert_eq!(p.backup_refs, vec!["main"]);
        assert!(p.github.is_none());
    }

    #[test]
    fn empty_document_is_all_defaults() {
        let p = RepoPolicy::parse("{}").unwrap();
        assert!(p.backup_refs.is_empty());
        assert!(p.github.is_none());
    }

    #[test]
    fn empty_and_whitespace_only_files_are_defaults() {
        // A 0-byte or whitespace/comment-only committed .git-ark.yml is a valid
        // "no policy", not a parse error.
        for body in ["", "   \n\t  \n", "# just a comment\n"] {
            let p = RepoPolicy::parse(body).unwrap();
            assert!(p.backup_refs.is_empty(), "backup_refs for {body:?}");
            assert!(p.github.is_none(), "github for {body:?}");
        }
    }

    #[test]
    fn read_repo_policy_none_when_no_repo() {
        // A path that is not a git repo → git show fails → Ok(None).
        let d = tempfile::tempdir().unwrap();
        let got = read_repo_policy(d.path()).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn read_repo_policy_reads_committed_file() {
        // A real repo with a committed .git-ark.yml is read back from HEAD.
        let work = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(work.path())
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(
            work.path().join(".git-ark.yml"),
            "backup_refs:\n  - main\ngithub:\n  owner: acme\n  branches:\n    - main\n",
        )
        .unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "add policy"]);

        let p = read_repo_policy(work.path()).unwrap().unwrap();
        assert_eq!(p.backup_refs, vec!["main"]);
        let g = p.github.unwrap();
        assert_eq!(g.owner, "acme");
        assert_eq!(g.branches, vec!["main"]);
        // Defaults still applied through the git-read path.
        assert!(g.enabled);
        assert_eq!(g.visibility, Visibility::Private);
    }
}
