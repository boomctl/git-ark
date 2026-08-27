use crate::config::Config;
use crate::{git, hooks};
use anyhow::{bail, Context, Result};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitVerb {
    ReceivePack,
    UploadPack,
    UploadArchive,
}

impl GitVerb {
    pub fn binary(&self) -> &'static str {
        match self {
            GitVerb::ReceivePack => "git-receive-pack",
            GitVerb::UploadPack => "git-upload-pack",
            GitVerb::UploadArchive => "git-upload-archive",
        }
    }

    pub fn is_write(&self) -> bool {
        matches!(self, GitVerb::ReceivePack)
    }
}

#[derive(Debug, Clone)]
pub struct GitCommand {
    pub verb: GitVerb,
    pub repo_arg: String,
}

pub fn parse_ssh_command(cmd: &str) -> Result<GitCommand> {
    let tokens = shell_words::split(cmd).context("splitting SSH_ORIGINAL_COMMAND")?;
    if tokens.len() != 2 {
        bail!("expected `<git-verb> <repo>`, got {} token(s): {cmd:?}", tokens.len());
    }
    let verb = match tokens[0].as_str() {
        "git-receive-pack" => GitVerb::ReceivePack,
        "git-upload-pack" => GitVerb::UploadPack,
        "git-upload-archive" => GitVerb::UploadArchive,
        other => bail!("disallowed command: {other:?}"),
    };
    Ok(GitCommand {
        verb,
        repo_arg: tokens[1].clone(),
    })
}

/// Return the first `dir.join(name)` in `dirs` that is a regular file.
fn find_in_dirs(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter().map(|d| d.join(name)).find(|p| p.is_file())
}

/// Resolve a git plumbing binary (e.g. `git-receive-pack`) to an absolute path.
///
/// Under an sshd forced command `$PATH` is often minimal or empty, so a bare
/// `Command::new("git-receive-pack")` can fail to find the binary even though
/// git is installed. Search `$PATH` first, then a fixed set of standard
/// locations, and hand `exec` an absolute path.
fn resolve_git_binary(name: &str) -> Result<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for fallback in ["/usr/bin", "/bin", "/usr/local/bin", "/opt/homebrew/bin"] {
        dirs.push(PathBuf::from(fallback));
    }
    find_in_dirs(name, &dirs).ok_or_else(|| {
        anyhow::anyhow!(
            "could not locate git plumbing binary {name:?} in $PATH or standard \
             directories (/usr/bin, /bin, /usr/local/bin, /opt/homebrew/bin); \
             ensure git is installed on the host"
        )
    })
}

pub fn resolve_repo_path(repos_root: &Path, raw: &str) -> Result<PathBuf> {
    let mut s = raw.trim();
    s = s.trim_start_matches('/');
    if let Some(rest) = s.strip_prefix("~/") {
        s = rest;
    }
    s = s.trim_start_matches('/');
    if s.is_empty() {
        bail!("empty repo path");
    }

    let mut rel = PathBuf::new();
    let mut last = String::new();
    for comp in s.split('/') {
        if comp.is_empty() || comp == "." {
            continue;
        }
        if comp == ".." {
            bail!("path traversal is not allowed: {raw:?}");
        }
        rel.push(comp);
        last = comp.to_string();
    }
    if last.is_empty() {
        bail!("empty repo path after normalization: {raw:?}");
    }
    if !last.ends_with(".git") {
        rel.set_file_name(format!("{last}.git"));
    }
    Ok(repos_root.join(rel))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_receive_pack() {
        let c = parse_ssh_command("git-receive-pack 'myproject.git'").unwrap();
        assert!(matches!(c.verb, GitVerb::ReceivePack));
        assert_eq!(c.repo_arg, "myproject.git");
        assert!(c.verb.is_write());
    }

    #[test]
    fn parses_upload_pack() {
        let c = parse_ssh_command("git-upload-pack 'a/b.git'").unwrap();
        assert!(matches!(c.verb, GitVerb::UploadPack));
        assert!(!c.verb.is_write());
    }

    #[test]
    fn rejects_non_git_command() {
        assert!(parse_ssh_command("rm -rf /").is_err());
        assert!(parse_ssh_command("scp x y").is_err());
    }

    #[test]
    fn rejects_wrong_arity() {
        assert!(parse_ssh_command("git-receive-pack").is_err());
        assert!(parse_ssh_command("git-receive-pack a b").is_err());
    }

    #[test]
    fn rejects_disallowed_verb_with_correct_arity() {
        // 2 tokens (correct arity) but the verb is not whitelisted → must be rejected by the whitelist, not the arity check.
        assert!(parse_ssh_command("git-fake 'repo.git'").is_err());
        assert!(parse_ssh_command("bash 'repo.git'").is_err());
    }

    #[test]
    fn find_in_dirs_returns_absolute_hit_and_none_when_absent() {
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join("git-receive-pack");
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
        let dirs = vec![PathBuf::from("/nonexistent-dir-xyz"), d.path().to_path_buf()];

        let found = find_in_dirs("git-receive-pack", &dirs).unwrap();
        assert_eq!(found, bin);
        assert!(found.is_absolute(), "resolved path must be absolute: {found:?}");

        assert!(
            find_in_dirs("git-upload-pack", &dirs).is_none(),
            "absent binary must resolve to None"
        );
    }
}

#[derive(Debug)]
pub enum Action {
    InitThenServe { repo: PathBuf, verb: GitVerb },
    Serve { repo: PathBuf, verb: GitVerb },
    Reject(String),
}

pub fn plan_action(cfg: &Config, cmd: &GitCommand) -> Action {
    let repo = match resolve_repo_path(&cfg.repos_root, &cmd.repo_arg) {
        Ok(r) => r,
        Err(e) => return Action::Reject(e.to_string()),
    };
    let exists = repo.exists();
    match (cmd.verb.is_write(), exists) {
        (_, true) => Action::Serve { repo, verb: cmd.verb },
        (true, false) => Action::InitThenServe { repo, verb: cmd.verb },
        (false, false) => Action::Reject(format!("repository does not exist: {}", cmd.repo_arg)),
    }
}

/// Entry point for the SSH forced command. Reads `SSH_ORIGINAL_COMMAND`, plans the
/// action, performs any side effects (bare-repo init, hook install), then execs the
/// real git plumbing. Only returns if the exec itself fails.
pub fn run_shell(cfg: &Config, config_path: &Path, binary: &Path) -> Result<()> {
    let raw = std::env::var("SSH_ORIGINAL_COMMAND")
        .context("SSH_ORIGINAL_COMMAND is not set (this runs as an SSH forced command)")?;
    let cmd = parse_ssh_command(&raw)?;
    let (repo, verb) = match plan_action(cfg, &cmd) {
        Action::Reject(why) => bail!("{why}"),
        Action::Serve { repo, verb } => (repo, verb),
        Action::InitThenServe { repo, verb } => {
            git::init_bare(&repo)?;
            (repo, verb)
        }
    };
    if verb.is_write() {
        hooks::install_post_receive(&repo, binary, config_path)?;
    }
    // Resolve to an absolute path first — a forced command's $PATH may be empty.
    let abs = resolve_git_binary(verb.binary())?;
    // exec replaces this process; only returns on failure.
    let err = Command::new(&abs).arg(&repo).exec();
    bail!("failed to exec {}: {err}", abs.display());
}

#[cfg(test)]
mod plan_tests {
    use super::*;
    use crate::config::{Config, S3Config, GithubConfig};

    fn cfg(root: &std::path::Path) -> Config {
        Config {
            repos_root: root.to_path_buf(),
            age_recipient: "age1x".into(),
            s3: S3Config { bucket: "b".into(), region: "r".into(), prefix: "git-ark".into() },
            github: GithubConfig::default(),
            backup_refs: Vec::new(),
        }
    }

    #[test]
    fn push_to_missing_repo_inits() {
        let d = tempfile::tempdir().unwrap();
        let c = cfg(d.path());
        let cmd = GitCommand { verb: GitVerb::ReceivePack, repo_arg: "new".into() };
        match plan_action(&c, &cmd) {
            Action::InitThenServe { repo, verb } => {
                assert_eq!(verb, GitVerb::ReceivePack);
                assert_eq!(repo, d.path().join("new.git"));
            }
            other => panic!("expected InitThenServe, got {other:?}"),
        }
    }

    #[test]
    fn fetch_from_missing_repo_rejects() {
        let d = tempfile::tempdir().unwrap();
        let c = cfg(d.path());
        let cmd = GitCommand { verb: GitVerb::UploadPack, repo_arg: "ghost".into() };
        assert!(matches!(plan_action(&c, &cmd), Action::Reject(_)));
    }

    #[test]
    fn serve_existing_repo() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("here.git");
        std::fs::create_dir_all(&repo).unwrap();
        let c = cfg(d.path());
        let cmd = GitCommand { verb: GitVerb::UploadPack, repo_arg: "here".into() };
        assert!(matches!(plan_action(&c, &cmd), Action::Serve { .. }));
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn strips_leading_slash_and_tilde() {
        let root = Path::new("/srv/repos");
        assert_eq!(resolve_repo_path(root, "/myproject.git").unwrap(), root.join("myproject.git"));
        assert_eq!(resolve_repo_path(root, "~/myproject.git").unwrap(), root.join("myproject.git"));
        assert_eq!(resolve_repo_path(root, "myproject.git").unwrap(), root.join("myproject.git"));
    }

    #[test]
    fn appends_git_suffix() {
        let root = Path::new("/srv/repos");
        assert_eq!(resolve_repo_path(root, "myproject").unwrap(), root.join("myproject.git"));
    }

    #[test]
    fn allows_subdirs() {
        let root = Path::new("/srv/repos");
        assert_eq!(resolve_repo_path(root, "team/app").unwrap(), root.join("team/app.git"));
    }

    #[test]
    fn rejects_traversal() {
        let root = Path::new("/srv/repos");
        assert!(resolve_repo_path(root, "../../etc/passwd").is_err());
        assert!(resolve_repo_path(root, "a/../../b").is_err());
        assert!(resolve_repo_path(root, "").is_err());
    }
}
